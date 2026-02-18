use crate::command::CommandInput;

/// Parse raw input string into a CommandInput.
/// Format: "<prefix> <args>" or just "<prefix>".
/// Examples:
///   "web rust programming" → prefix: "web", args: "rust programming"
///   "open firefox"         → prefix: "open", args: "firefox"
///   "run ls -la"           → prefix: "run", args: "ls -la"
pub fn parse(raw: &str) -> CommandInput {
    let trimmed = raw.trim();
    match trimmed.split_once(' ') {
        Some((prefix, args)) => CommandInput {
            prefix: prefix.to_lowercase(),
            args: args.to_string(),
            raw: trimmed.to_string(),
        },
        None => CommandInput {
            prefix: trimmed.to_lowercase(),
            args: String::new(),
            raw: trimmed.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_args() {
        let input = parse("web rust programming");
        assert_eq!(input.prefix, "web");
        assert_eq!(input.args, "rust programming");
        assert_eq!(input.raw, "web rust programming");
    }

    #[test]
    fn parse_without_args() {
        let input = parse("help");
        assert_eq!(input.prefix, "help");
        assert_eq!(input.args, "");
    }

    #[test]
    fn parse_trims_whitespace() {
        let input = parse("  open  firefox  ");
        assert_eq!(input.prefix, "open");
        assert_eq!(input.args, " firefox");
        assert_eq!(input.raw, "open  firefox");
    }

    #[test]
    fn parse_lowercases_prefix() {
        let input = parse("WEB google");
        assert_eq!(input.prefix, "web");
    }
}
