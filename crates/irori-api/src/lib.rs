//! Which HTTP routes a program's token may call (`docs/specs/api.md`).
//!
//! The page does not use this. It signs in with a session cookie; `needs` in the server
//! says what that cookie may do. A token is checked against the scopes it was given.
//! The websocket and the routes themselves live in the `irori` binary, next to the
//! session store.

use irori_types::ApiScope;

/// The scopes a token must hold for this request.
///
/// `None` means a token may not call it. Setup stays with a person on the page.
/// `path` is the request path without a query string.
pub fn scopes_for(method: &str, path: &str) -> Option<&'static [ApiScope]> {
    let path = path.split('?').next().unwrap_or(path);
    if path == "/api/ws" {
        return Some(&[
            ApiScope::RegistryRead,
            ApiScope::StatesRead,
            ApiScope::EventsRead,
        ]);
    }
    if method.eq_ignore_ascii_case("POST") && path == "/api/command" {
        return Some(&[ApiScope::ServicesCall]);
    }
    let reading = method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD");
    if !reading {
        return None;
    }
    if path == "/api/home" {
        return Some(&[ApiScope::RegistryRead, ApiScope::StatesRead]);
    }
    if path == "/api/states" {
        return Some(&[ApiScope::StatesRead]);
    }
    if path == "/api/history" || path.starts_with("/api/history/") {
        return Some(&[ApiScope::HistoryRead]);
    }
    if matches!(
        path,
        "/api/devices"
            | "/api/entities"
            | "/api/areas"
            | "/api/floors"
            | "/api/floorplan"
            | "/api/extensions"
            | "/api/apps"
    ) {
        return Some(&[ApiScope::RegistryRead]);
    }
    None
}

/// The first required scope `have` does not contain.
pub fn missing_scope(need: &[ApiScope], have: &[ApiScope]) -> Option<ApiScope> {
    need.iter().copied().find(|scope| !have.contains(scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_can_read_and_control_and_nothing_else() {
        let read = scopes_for("GET", "/api/states").expect("states");
        assert_eq!(read, &[ApiScope::StatesRead]);
        assert!(scopes_for("GET", "/api/states?x=1").is_some());
        assert_eq!(
            scopes_for("GET", "/api/home").expect("home").len(),
            2,
            "the whole picture needs the registry and the states"
        );
        assert_eq!(
            scopes_for("POST", "/api/command"),
            Some([ApiScope::ServicesCall].as_slice())
        );
        assert_eq!(scopes_for("GET", "/api/ws").expect("socket").len(), 3);
        assert_eq!(
            scopes_for("GET", "/api/history/light.hall"),
            Some([ApiScope::HistoryRead].as_slice())
        );
        // Setup is not a scope. A token is refused these.
        for (method, path) in [
            ("POST", "/api/tokens"),
            ("DELETE", "/api/tokens/tablet"),
            ("POST", "/api/restart"),
            ("PUT", "/api/floorplan"),
            ("POST", "/api/areas"),
            ("GET", "/api/system"),
            ("GET", "/api/extensions/demo/log"),
        ] {
            assert_eq!(scopes_for(method, path), None, "{method} {path}");
        }
    }

    #[test]
    fn the_missing_scope_is_named() {
        let need = scopes_for("GET", "/api/home").expect("home");
        assert_eq!(
            missing_scope(need, &[ApiScope::RegistryRead]),
            Some(ApiScope::StatesRead)
        );
        assert_eq!(missing_scope(need, need), None);
    }
}
