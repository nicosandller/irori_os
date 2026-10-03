//! How long one view's conversation may get, and how the house's conversations share a cap.

/// Messages kept for one view (the general chat, one device, or one automation).
pub const LIMIT_MESSAGES: usize = 40;
/// Text kept for one view.
pub const LIMIT_BYTES: usize = 64 * 1024;
/// Text kept across every view.
pub const LIMIT_GLOBAL_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub body: String,
}

/// Drops the oldest turns in `scope` until it is within both limits. A single turn longer than
/// the byte cap is cut down to the cap, so one paste cannot pin the whole view.
pub fn append_capped(turns: &mut Vec<Turn>, turn: Turn) {
    let mut turn = turn;
    if turn.body.len() > LIMIT_BYTES {
        let mut end = LIMIT_BYTES;
        while !turn.body.is_char_boundary(end) {
            end -= 1;
        }
        turn.body.truncate(end);
    }
    turns.push(turn);
    while turns.len() > LIMIT_MESSAGES || byte_len(turns) > LIMIT_BYTES {
        turns.remove(0);
    }
}

pub fn byte_len(turns: &[Turn]) -> usize {
    turns.iter().map(|turn| turn.body.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Turn {
        Turn {
            role: Role::User,
            body: text.to_owned(),
        }
    }

    #[test]
    fn the_oldest_turn_goes_once_the_count_is_past_the_cap() {
        let mut turns = Vec::new();
        for n in 0..LIMIT_MESSAGES {
            append_capped(&mut turns, user(&n.to_string()));
        }
        append_capped(&mut turns, user("newest"));
        assert_eq!(turns.len(), LIMIT_MESSAGES);
        assert_eq!(turns[0].body, "1");
        assert_eq!(turns[turns.len() - 1].body, "newest");
    }

    #[test]
    fn a_huge_paste_is_cut_and_does_not_keep_the_older_turns() {
        let mut turns = vec![user("keep me if you can")];
        append_capped(&mut turns, user(&"x".repeat(LIMIT_BYTES + 10)));
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].body.len(), LIMIT_BYTES);
    }
}
