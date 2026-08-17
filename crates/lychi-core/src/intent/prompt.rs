//! Action descriptions for the registry-coverage check.
//!
//! This module was once the home of the legacy AI intent-routing prompt — the
//! system-prompt builder and the JSON response parser that turned a model reply
//! into a route or a multi-step plan. That whole path is gone: natural language
//! is owned entirely by the streaming tool-calling agent (`coordinator/`), which
//! talks to the model through the one `chat()` primitive. No prompt builder, no
//! response parser, no `route`/`plan` types.
//!
//! What survives is the single piece with a live, non-AI consumer: the
//! `action_description` table. The app's startup validation (`state.rs`) reads
//! it to assert every registered handler carries a human description, so a new
//! handler added without one fails the build rather than shipping undocumented.

/// Does this command have a real description?
///
/// Public so the crate that BUILDS the registry can assert every registered
/// handler is covered — `lychi-core` cannot see the registration list itself.
pub fn has_action_description(id: &str) -> bool {
    action_description(id) != "Unknown command"
}

fn action_description(id: &str) -> &'static str {
    match id {
        "ask" => {
            "Ask a question and get an AI-powered inline answer. Use for direct questions (what, who, how, why, explain, define). Prefer over 'web' when the user is asking a question"
        }
        "open" => "Launch a desktop application by name (e.g. firefox, spotify, vscode)",
        "web" => "Search the web for general lookups or non-question searches",
        "yt" => "Search YouTube for videos",
        "run" => "Execute a shell command (e.g. 'code .', 'ls ~', 'htop')",
        "calc" => "Evaluate a math expression (e.g. '2+2', 'sqrt(144)')",
        "browse" => {
            "Browse a directory interactively. ONLY use when the user wants to open/browse a whole folder without filtering (e.g. 'browse downloads', 'show my documents folder'). If the user mentions specific filenames or search terms, use 'run' with ls/find instead"
        }
        "file" => "Open a file or directory in the default app (e.g. '~/Downloads')",
        "url" => "Open a URL in the browser (e.g. 'https://github.com')",
        "media" => {
            "Control media players. Args: play, pause, next, prev, toggle, 'pause all'. Prefix with provider to target a specific player (e.g. 'spotify pause', 'yt next'). Use for any media/music control"
        }
        "project" => {
            "Open a project folder by name in the code editor (e.g. 'readyroos', 'lychi'). Use when the user wants to open a project in VSCode/editor by its name"
        }
        "sysinfo" => {
            "System info — show IP address, CPU, memory, or disk usage. Subcommands: ip, cpu, mem, disk. Empty args shows a full overview"
        }
        "system" => {
            "System controls. Args: shutdown, reboot, suspend, hibernate, lock, logout, mute, unmute, volume <up|down|0-100>, brightness <up|down|0-100>, wifi <on|off>, bluetooth <on|off>, connect bluetooth <device>, disconnect bluetooth <device>, shutdown in <duration> (e.g. 'shutdown in 30m'), cancel shutdown"
        }
        "note" => {
            "Notes list (max 5). Args: text to add a note, 'read' to view all, 'delete <id>' to remove"
        }
        "todo" => "Todo list. Args: 'add <text>', 'list', 'done <id>', 'delete <id>', 'summary'",
        "weather" => "Get current weather/forecast. Args: city name, or empty for default location",
        "weather-ask" => {
            "Answer conversational weather questions with real data. Args: the full question (e.g. 'will it rain today', 'do I need a jacket')"
        }
        "timer" => {
            "Timer and stopwatch. Args: 'start [name] <duration>' (e.g. 'start 25m', 'start workout 5m'), 'stopwatch [name]' to start a count-up stopwatch, 'stop [name]', 'pause [name]', 'resume [name]', 'status', 'clear'. Shorthand: bare duration like '25m' starts a timer"
        }
        "time" => {
            "Show current time in a timezone or convert between timezones. Args: timezone name or city (e.g. 'tokyo', 'EST', 'london'). Empty for local time"
        }
        "alias" => {
            "Manage command aliases. Args: 'add <name> <command>', 'update <name> <command>', 'delete <name>', 'list'. Creates shortcuts for common commands"
        }
        "reminder" => {
            "Set timed reminders with desktop notifications. Args: 'add <text> in/at <time>' (e.g. 'add buy milk in 30m', 'add standup at 9am', 'add meeting tomorrow 2pm'), 'list', 'delete <id>', 'clear'. Without 'add', infers from natural language"
        }
        "appctl" => "Focus, quit, or kill a running application by name",
        "bm" => "Search and open browser bookmarks by keyword",
        "clip" => "Browse and paste from clipboard history",
        "emoji" => "Search and copy an emoji by name or keyword",
        "snip" => "Snippets — save and paste reusable text blocks",
        "sym" => "Search and copy a symbol or special character by name",
        "unicode" => "Search Unicode characters by name or codepoint",
        "devutil" => {
            "Developer text utilities. Prepend the verb to the text. Verbs: 'base64 <text>' / 'base64 -d <b64>', 'hash [md5|sha256] <text>', 'urlencode <text>' / 'urldecode <text>', 'epoch [<unix-seconds>]', 'json <text>' (pretty-print) / 'json -m <text>' (minify), 'upper <text>' / 'lower <text>' / 'title <text>' (change case), 'slug <text>' (url-safe slug), 'reverse <text>', 'count <text>' (chars/words/lines). Use for 'encode/decode', 'make uppercase', 'format this json', 'slugify', etc."
        }
        "generate" => {
            "Generate random values. Args: 'password [length]', 'uuid', 'token [length]', 'random [min] <max>' (random integer, default 0–100). Use for 'generate a password', 'give me a uuid', 'random number between 1 and 100', 'roll a dice'"
        }
        "color" => "Convert or inspect a color between HEX, RGB, and HSL (e.g. '#ff5733')",
        "screenshot" => {
            "Take a screenshot. Args: empty or 'full' for the whole screen, 'area' (aliases: region, select) to select a region, 'window' for the active window. Use for 'take a screenshot', 'capture this region', 'grab a screenshot of the window'"
        }
        "services" => "List currently running systemd services",
        "service" => {
            "Control a systemd service. Args: '<name>' or '<name> status' to check it; '<name> start|stop|restart|reload|enable|disable' to control it. Use for 'restart nginx', 'is docker running', 'stop the bluetooth service'"
        }
        "packages" => {
            "Search or install SYSTEM packages via the OS package manager (dnf/apt/pacman/flatpak). Args: 'search <query>' or 'install <package>'. Use for 'install neovim', 'search for a markdown editor', 'is ripgrep available to install'. NOT for web searches"
        }
        // --- Added 2026-08-07: these were registered but undescribed, so the
        // --- model saw "Unknown command" and could never route to them.
        // --- Text mirrors each handler's own `description()`.
        "win" => {
            "Switch between open windows — focus or close a running application's window (e.g. 'win firefox'). Use when the user wants to switch TO something already open rather than launch it"
        }
        "ssh" => "Connect to an SSH host from ~/.ssh/config (e.g. 'ssh myserver')",
        "define" => "Look up a word's dictionary definition (e.g. 'define ephemeral')",
        "qr" => "Generate a QR code from text or a URL",
        "zip" => "Create an archive: zip <path...> [to <out.zip>]",
        "extract" => "Extract an archive: extract <archive.zip|.tar.gz> [to <dir>]",
        "convert" => {
            "Convert an image to another format: convert <path> to <png|jpg|webp|gif|bmp|tiff>"
        }
        "resize" => "Resize an image: resize <path> to <800x600|800|x600|50%>",
        "clear" => {
            "Clear stored data — history, clipboard, or learned suggestions. Destructive, so it always confirms first"
        }
        "quicklink" => "Run a user-defined search shortcut (gh, npm, mdn, …)",
        "script" => "Run a user Script Command from ~/.config/lychi/scripts/",
        "pin_workspace" => "Pin a workspace directory so context detection uses it",
        "ctx" => "Show the current environment context (debugging aid)",

        // A handler with no entry here is unknown to this table. The startup
        // validation in `state.rs` fails when a registered handler is missing
        // here, so it cannot go unnoticed.
        _ => "Unknown command",
    }
}
