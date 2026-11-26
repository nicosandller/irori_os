//! A flow brought over from Home Assistant and its twin there (issue #199).
//!
//! The owner turning such a flow on hands the job over: once the page has warned them and they
//! confirmed (`hand_over: true`), the Home Assistant extension switches that one automation off,
//! and only then is the flow turned on. If Home Assistant refuses or can't be reached, the flow
//! stays off and the page says why. Turning the flow off or deleting it does that first, then
//! asks for the automation to be switched back on; if Home Assistant can't be reached just then,
//! the Home Assistant extension keeps it on its list and switches it on once it can.
//!
//! Only the home's owner gets here: a flow brought over is the home's. Everything else about
//! such a flow goes the usual way ([`crate::rpc`]), which never turns one on: saving it keeps
//! whether it's on as it was.

use irori_flow_types::HomeAssistantTwin;
use irori_protocol::engine::{AppRequest, EngineClient};
use irori_types::{ExtensionId, RuleId};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::home_assistant::Fetched;
use crate::{Service, now};

/// The extension that switches the twin.
const BRIDGE: &str = "homeassistant";

/// The page's word that the owner read the warning.
const CONFIRMED: &str = "hand_over";

/// Whether `request` is about a flow with a twin, for the owner: turning it on or off, or
/// deleting it.
pub fn takes(service: &Service, request: &AppRequest) -> bool {
    request.caller.owner
        && matches!(request.method.as_str(), "flows.enable" | "flows.delete")
        && twin(service, &request.params).is_some()
}

/// The flow a question names, and its twin.
fn twin(service: &Service, params: &Value) -> Option<(RuleId, HomeAssistantTwin)> {
    let id = RuleId::try_from(params["id"].as_str()?).ok()?;
    let twin = service.store.flow(&id)?.home_assistant.clone()?;
    Some((id, twin))
}

/// Asks the Home Assistant extension `method` about `entity_id`, beside the loop.
fn ask(
    client: &EngineClient,
    method: &'static str,
    entity_id: String,
    then: impl FnOnce(Result<Value, String>) -> Fetched + Send + 'static,
    fetched: &mpsc::UnboundedSender<Fetched>,
) {
    let client = client.clone();
    let fetched = fetched.clone();
    tokio::spawn(async move {
        let bridge = ExtensionId::try_from(BRIDGE).expect("a valid id");
        let answer = client
            .ask_extension(bridge, method, json!({ "entity_id": entity_id }))
            .await
            .map_err(|why| why.to_string());
        let _ = fetched.send(then(answer));
    });
}

/// Asks for `entity_id` to be switched back on, answering `request` (if any) once it's asked.
pub fn give_back(service: &Service, entity_id: String, request: Option<AppRequest>, done: Value) {
    // A home without the Home Assistant extension: removing it switched everything back on.
    if !crate::rpc::bridged(&service.registry) {
        if let Some(request) = request {
            request.answer(Ok(done));
        }
        return;
    }
    ask(
        &service.client,
        "twin.give_back",
        entity_id,
        move |answer| Fetched::GaveBack {
            request,
            done,
            answer,
        },
        &service.fetched_tx,
    );
}

/// Starts on the owner's question about a flow with a twin.
pub fn start(service: &mut Service, request: AppRequest) {
    let Some((id, twin)) = twin(service, &request.params) else {
        request.answer(Err("there's no such flow".to_owned()));
        return;
    };
    if service.twins_underway.contains(&id) {
        request.answer(Err(
            "Home Assistant is still being asked about this flow's automation; try again in a moment"
                .to_owned(),
        ));
        return;
    }
    let on = request.params["enabled"].as_bool();
    match (request.method.as_str(), on) {
        ("flows.enable", Some(true)) => {
            if service.store.flow(&id).is_some_and(|flow| flow.enabled) {
                request.answer(Ok(Value::Null));
                return;
            }
            if request.params[CONFIRMED] != json!(true) {
                request.answer(Err(format!(
                    "turning this flow on switches off “{}” in Home Assistant; turn it on from \
                     the Automations page, which asks you first",
                    twin.entity_id
                )));
                return;
            }
            if !crate::rpc::bridged(&service.registry) {
                request.answer(Err(
                    "the Home Assistant extension isn't here to switch off its automation, so \
                     the flow stays off"
                        .to_owned(),
                ));
                return;
            }
            service.twins_underway.insert(id.clone());
            let entity_id = twin.entity_id.clone();
            ask(
                &service.client,
                "twin.hand_over",
                twin.entity_id,
                move |answer| Fetched::HandedOver {
                    request,
                    flow: id,
                    entity_id,
                    answer,
                },
                &service.fetched_tx,
            );
        }
        ("flows.enable", Some(false)) => {
            // Off in Irori first: the two never both stop running.
            let turned_off = service.store.flow(&id).cloned().map(|mut flow| {
                let was = flow.enabled;
                flow.enabled = false;
                (was, flow)
            });
            if let Some((true, flow)) = turned_off {
                if let Err(why) = service.store.save(&flow, now(), None) {
                    request.answer(Err(why));
                    return;
                }
                service.arm();
            }
            give_back(service, twin.entity_id, Some(request), Value::Null);
        }
        ("flows.delete", _) => {
            if let Err(why) = service.store.delete(&id) {
                request.answer(Err(why));
                return;
            }
            service.arm();
            give_back(service, twin.entity_id, Some(request), Value::Null);
        }
        _ => request.answer(Err("say whether to turn it on: `enabled`".to_owned())),
    }
}

/// Home Assistant answered switching the twin off: the flow is turned on, or stays off.
pub fn handed_over(
    service: &mut Service,
    request: AppRequest,
    id: RuleId,
    entity_id: String,
    answer: Result<Value, String>,
) {
    service.twins_underway.remove(&id);
    let answer = match answer {
        Ok(answer) => answer,
        Err(why) => {
            request.answer(Err(format!(
                "Home Assistant's automation wasn't switched off, so the flow stays off: {why}"
            )));
            return;
        }
    };
    let turned_on = service.store.flow(&id).cloned().map(|mut flow| {
        flow.enabled = true;
        flow
    });
    let saved = match turned_on {
        Some(flow) => service.store.save(&flow, now(), None),
        None => Err("the flow was deleted meanwhile".to_owned()),
    };
    match saved {
        Ok(()) => {
            service.arm();
            request.answer(Ok(answer));
        }
        Err(why) => {
            // Not on in Irori after all: Home Assistant runs it again.
            give_back(service, entity_id, None, Value::Null);
            request.answer(Err(format!("the flow couldn't be turned on: {why}")));
        }
    }
}

/// Home Assistant answered switching the twin back on. Irori's side is done either way: one
/// not switched on yet is waited for by the Home Assistant extension, and the page says so.
pub fn gave_back(request: Option<AppRequest>, done: Value, answer: Result<Value, String>) {
    let Some(request) = request else {
        if let Err(why) = answer {
            tracing::warn!(%why, "couldn't ask for Home Assistant's automation back");
        }
        return;
    };
    let mut done = match done {
        Value::Null => json!({}),
        other => other,
    };
    match answer {
        Ok(answer) => {
            if let Some(waiting) = answer.get("waiting") {
                done["home_assistant_waiting"] = waiting.clone();
            }
        }
        Err(why) => {
            done["home_assistant_waiting"] = json!(format!(
                "the Home Assistant extension couldn't be asked to switch it back on ({why}); \
                 use Restore under Bring over from Home Assistant once it's running"
            ));
        }
    }
    request.answer(Ok(done));
}
