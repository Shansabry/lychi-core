use async_trait::async_trait;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::sync::Mutex;

use crate::action_registry::handlers::clipboard::write_to_clipboard;
use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

/// Cached nucleo matcher.
static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

struct SymbolEntry {
    ch: &'static str,
    name: &'static str,
    keywords: &'static str,
    category: &'static str,
}

// Popular symbols shown on empty query
const POPULAR_INDICES: &[usize] = &[
    0,   // →
    1,   // ←
    4,   // ↑
    5,   // ↓
    60,  // ∞
    61,  // ≈
    62,  // ≠
    65,  // ×
    66,  // ÷
    77,  // °
    95,  // ©
    96,  // ®
    97,  // ™
    63,  // ±
    71,  // √
    100, // •
    102, // —
    103, // …
    112, // ✓
    113, // ✗
];

const SYMBOLS: &[SymbolEntry] = &[
    // ── Arrows ──
    SymbolEntry {
        ch: "→",
        name: "right arrow",
        keywords: "arrow right direction",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "←",
        name: "left arrow",
        keywords: "arrow left direction",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↔",
        name: "left right arrow",
        keywords: "arrow horizontal both",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↕",
        name: "up down arrow",
        keywords: "arrow vertical both",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↑",
        name: "up arrow",
        keywords: "arrow up direction",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↓",
        name: "down arrow",
        keywords: "arrow down direction",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↗",
        name: "north east arrow",
        keywords: "arrow diagonal up right",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↘",
        name: "south east arrow",
        keywords: "arrow diagonal down right",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↙",
        name: "south west arrow",
        keywords: "arrow diagonal down left",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↖",
        name: "north west arrow",
        keywords: "arrow diagonal up left",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⇒",
        name: "double right arrow",
        keywords: "arrow implies then",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⇐",
        name: "double left arrow",
        keywords: "arrow double left",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⇑",
        name: "double up arrow",
        keywords: "arrow double up",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⇓",
        name: "double down arrow",
        keywords: "arrow double down",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⇔",
        name: "double left right arrow",
        keywords: "arrow iff equivalent",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⟶",
        name: "long right arrow",
        keywords: "arrow long maps to",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⟵",
        name: "long left arrow",
        keywords: "arrow long left",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⟷",
        name: "long left right arrow",
        keywords: "arrow long both",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↩",
        name: "right arrow curving left",
        keywords: "arrow return undo",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "↪",
        name: "left arrow curving right",
        keywords: "arrow redo forward",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⤴",
        name: "right arrow curving up",
        keywords: "arrow curve up",
        category: "Arrows",
    },
    SymbolEntry {
        ch: "⤵",
        name: "right arrow curving down",
        keywords: "arrow curve down",
        category: "Arrows",
    },
    // ── Currency ──
    SymbolEntry {
        ch: "$",
        name: "dollar sign",
        keywords: "currency money usd",
        category: "Currency",
    },
    SymbolEntry {
        ch: "€",
        name: "euro sign",
        keywords: "currency money eur europe",
        category: "Currency",
    },
    SymbolEntry {
        ch: "£",
        name: "pound sign",
        keywords: "currency money gbp british",
        category: "Currency",
    },
    SymbolEntry {
        ch: "¥",
        name: "yen sign",
        keywords: "currency money jpy yuan cny japan china",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₹",
        name: "indian rupee sign",
        keywords: "currency money inr india",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₩",
        name: "won sign",
        keywords: "currency money krw korea",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₽",
        name: "ruble sign",
        keywords: "currency money rub russia",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₿",
        name: "bitcoin sign",
        keywords: "currency money crypto btc",
        category: "Currency",
    },
    SymbolEntry {
        ch: "¢",
        name: "cent sign",
        keywords: "currency money cents",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₱",
        name: "peso sign",
        keywords: "currency money php philippines",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₫",
        name: "dong sign",
        keywords: "currency money vnd vietnam",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₺",
        name: "turkish lira sign",
        keywords: "currency money try turkey",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₴",
        name: "hryvnia sign",
        keywords: "currency money uah ukraine",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₸",
        name: "tenge sign",
        keywords: "currency money kzt kazakhstan",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₦",
        name: "naira sign",
        keywords: "currency money ngn nigeria",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₡",
        name: "colon sign",
        keywords: "currency money costa rica",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₮",
        name: "tugrik sign",
        keywords: "currency money mnt mongolia",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₭",
        name: "kip sign",
        keywords: "currency money lak laos",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₠",
        name: "euro-currency sign",
        keywords: "currency money ecu",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₢",
        name: "cruzeiro sign",
        keywords: "currency money brazil",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₣",
        name: "french franc sign",
        keywords: "currency money france",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₤",
        name: "lira sign",
        keywords: "currency money italy",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₥",
        name: "mill sign",
        keywords: "currency money tenth cent",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₧",
        name: "peseta sign",
        keywords: "currency money spain",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₨",
        name: "rupee sign",
        keywords: "currency money south asia",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₪",
        name: "new sheqel sign",
        keywords: "currency money ils israel",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₫",
        name: "dong sign",
        keywords: "currency money vnd vietnam",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₯",
        name: "drachma sign",
        keywords: "currency money greece",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₰",
        name: "german penny sign",
        keywords: "currency money pfennig",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₲",
        name: "guarani sign",
        keywords: "currency money pyg paraguay",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₳",
        name: "austral sign",
        keywords: "currency money argentina",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₵",
        name: "cedi sign",
        keywords: "currency money ghs ghana",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₶",
        name: "livre tournois sign",
        keywords: "currency money france historical",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₷",
        name: "spesmilo sign",
        keywords: "currency money esperanto",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₹",
        name: "indian rupee",
        keywords: "currency money inr india",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₼",
        name: "manat sign",
        keywords: "currency money azn azerbaijan",
        category: "Currency",
    },
    SymbolEntry {
        ch: "₾",
        name: "lari sign",
        keywords: "currency money gel georgia",
        category: "Currency",
    },
    // ── Math ──
    SymbolEntry {
        ch: "∞",
        name: "infinity",
        keywords: "math forever unlimited",
        category: "Math",
    },
    SymbolEntry {
        ch: "≈",
        name: "approximately equal",
        keywords: "math approx almost",
        category: "Math",
    },
    SymbolEntry {
        ch: "≠",
        name: "not equal",
        keywords: "math inequality different",
        category: "Math",
    },
    SymbolEntry {
        ch: "±",
        name: "plus minus",
        keywords: "math plus or minus",
        category: "Math",
    },
    SymbolEntry {
        ch: "≤",
        name: "less than or equal",
        keywords: "math comparison lte",
        category: "Math",
    },
    SymbolEntry {
        ch: "≥",
        name: "greater than or equal",
        keywords: "math comparison gte",
        category: "Math",
    },
    SymbolEntry {
        ch: "×",
        name: "multiplication sign",
        keywords: "math multiply times cross",
        category: "Math",
    },
    SymbolEntry {
        ch: "÷",
        name: "division sign",
        keywords: "math divide obelus",
        category: "Math",
    },
    SymbolEntry {
        ch: "·",
        name: "middle dot",
        keywords: "math dot product interpunct",
        category: "Math",
    },
    SymbolEntry {
        ch: "∓",
        name: "minus plus",
        keywords: "math minus or plus",
        category: "Math",
    },
    SymbolEntry {
        ch: "≡",
        name: "identical to",
        keywords: "math congruent triple equals",
        category: "Math",
    },
    SymbolEntry {
        ch: "≢",
        name: "not identical to",
        keywords: "math not congruent",
        category: "Math",
    },
    SymbolEntry {
        ch: "√",
        name: "square root",
        keywords: "math radical sqrt",
        category: "Math",
    },
    SymbolEntry {
        ch: "∛",
        name: "cube root",
        keywords: "math radical cbrt",
        category: "Math",
    },
    SymbolEntry {
        ch: "∑",
        name: "summation",
        keywords: "math sigma sum",
        category: "Math",
    },
    SymbolEntry {
        ch: "∏",
        name: "product",
        keywords: "math pi product",
        category: "Math",
    },
    SymbolEntry {
        ch: "∫",
        name: "integral",
        keywords: "math calculus integrate",
        category: "Math",
    },
    SymbolEntry {
        ch: "∬",
        name: "double integral",
        keywords: "math calculus surface",
        category: "Math",
    },
    SymbolEntry {
        ch: "∂",
        name: "partial derivative",
        keywords: "math calculus partial del",
        category: "Math",
    },
    SymbolEntry {
        ch: "°",
        name: "degree sign",
        keywords: "math temperature angle",
        category: "Math",
    },
    SymbolEntry {
        ch: "∇",
        name: "nabla",
        keywords: "math gradient del vector",
        category: "Math",
    },
    SymbolEntry {
        ch: "∈",
        name: "element of",
        keywords: "math set member belongs",
        category: "Math",
    },
    SymbolEntry {
        ch: "∉",
        name: "not element of",
        keywords: "math set not member",
        category: "Math",
    },
    SymbolEntry {
        ch: "⊂",
        name: "subset of",
        keywords: "math set subset proper",
        category: "Math",
    },
    SymbolEntry {
        ch: "⊃",
        name: "superset of",
        keywords: "math set superset proper",
        category: "Math",
    },
    SymbolEntry {
        ch: "⊆",
        name: "subset or equal",
        keywords: "math set subset",
        category: "Math",
    },
    SymbolEntry {
        ch: "⊇",
        name: "superset or equal",
        keywords: "math set superset",
        category: "Math",
    },
    SymbolEntry {
        ch: "∪",
        name: "union",
        keywords: "math set cup combine",
        category: "Math",
    },
    SymbolEntry {
        ch: "∩",
        name: "intersection",
        keywords: "math set cap overlap",
        category: "Math",
    },
    SymbolEntry {
        ch: "∅",
        name: "empty set",
        keywords: "math set null void",
        category: "Math",
    },
    SymbolEntry {
        ch: "∀",
        name: "for all",
        keywords: "math logic universal quantifier",
        category: "Math",
    },
    SymbolEntry {
        ch: "∃",
        name: "there exists",
        keywords: "math logic existential quantifier",
        category: "Math",
    },
    SymbolEntry {
        ch: "¬",
        name: "logical not",
        keywords: "math logic negation",
        category: "Math",
    },
    SymbolEntry {
        ch: "∧",
        name: "logical and",
        keywords: "math logic conjunction wedge",
        category: "Math",
    },
    SymbolEntry {
        ch: "∨",
        name: "logical or",
        keywords: "math logic disjunction vee",
        category: "Math",
    },
    SymbolEntry {
        ch: "⊕",
        name: "circled plus",
        keywords: "math xor direct sum",
        category: "Math",
    },
    SymbolEntry {
        ch: "⊗",
        name: "circled times",
        keywords: "math tensor product",
        category: "Math",
    },
    SymbolEntry {
        ch: "∝",
        name: "proportional to",
        keywords: "math proportion varies",
        category: "Math",
    },
    SymbolEntry {
        ch: "∴",
        name: "therefore",
        keywords: "math logic conclusion",
        category: "Math",
    },
    SymbolEntry {
        ch: "∵",
        name: "because",
        keywords: "math logic reason",
        category: "Math",
    },
    // ── Misc & Punctuation ──
    SymbolEntry {
        ch: "©",
        name: "copyright sign",
        keywords: "legal intellectual property",
        category: "Misc",
    },
    SymbolEntry {
        ch: "®",
        name: "registered sign",
        keywords: "legal trademark registered",
        category: "Misc",
    },
    SymbolEntry {
        ch: "™",
        name: "trade mark sign",
        keywords: "legal trademark",
        category: "Misc",
    },
    SymbolEntry {
        ch: "§",
        name: "section sign",
        keywords: "legal section paragraph",
        category: "Misc",
    },
    SymbolEntry {
        ch: "¶",
        name: "pilcrow sign",
        keywords: "paragraph mark",
        category: "Misc",
    },
    SymbolEntry {
        ch: "•",
        name: "bullet",
        keywords: "list dot point",
        category: "Misc",
    },
    SymbolEntry {
        ch: "◦",
        name: "white bullet",
        keywords: "list dot point hollow",
        category: "Misc",
    },
    SymbolEntry {
        ch: "—",
        name: "em dash",
        keywords: "dash long punctuation",
        category: "Misc",
    },
    SymbolEntry {
        ch: "–",
        name: "en dash",
        keywords: "dash medium range",
        category: "Misc",
    },
    SymbolEntry {
        ch: "…",
        name: "ellipsis",
        keywords: "dots three trailing",
        category: "Misc",
    },
    SymbolEntry {
        ch: "†",
        name: "dagger",
        keywords: "footnote cross obelisk",
        category: "Misc",
    },
    SymbolEntry {
        ch: "‡",
        name: "double dagger",
        keywords: "footnote cross diesis",
        category: "Misc",
    },
    SymbolEntry {
        ch: "‰",
        name: "per mille",
        keywords: "per thousand permille",
        category: "Misc",
    },
    SymbolEntry {
        ch: "‱",
        name: "per ten thousand",
        keywords: "basis point",
        category: "Misc",
    },
    SymbolEntry {
        ch: "№",
        name: "numero sign",
        keywords: "number hash",
        category: "Misc",
    },
    SymbolEntry {
        ch: "℃",
        name: "degree celsius",
        keywords: "temperature celsius centigrade",
        category: "Misc",
    },
    SymbolEntry {
        ch: "℉",
        name: "degree fahrenheit",
        keywords: "temperature fahrenheit",
        category: "Misc",
    },
    // ── Technical / UI ──
    SymbolEntry {
        ch: "✓",
        name: "check mark",
        keywords: "check tick yes done",
        category: "Technical",
    },
    SymbolEntry {
        ch: "✗",
        name: "ballot x",
        keywords: "cross no fail reject",
        category: "Technical",
    },
    SymbolEntry {
        ch: "✕",
        name: "multiplication x",
        keywords: "cross close delete",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⌘",
        name: "command",
        keywords: "mac apple cmd key",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⌥",
        name: "option",
        keywords: "mac apple alt key",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⇧",
        name: "shift",
        keywords: "mac apple shift key",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⌃",
        name: "control",
        keywords: "mac apple ctrl key",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⎋",
        name: "escape",
        keywords: "esc key",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⏎",
        name: "return",
        keywords: "enter key newline",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⌫",
        name: "delete left",
        keywords: "backspace erase key",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⌦",
        name: "delete right",
        keywords: "forward delete key",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⇥",
        name: "tab right",
        keywords: "tab key indent",
        category: "Technical",
    },
    SymbolEntry {
        ch: "␣",
        name: "space",
        keywords: "space bar blank key",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⏏",
        name: "eject",
        keywords: "eject media disc",
        category: "Technical",
    },
    SymbolEntry {
        ch: "☰",
        name: "hamburger menu",
        keywords: "menu three lines trigram",
        category: "Technical",
    },
    SymbolEntry {
        ch: "⚙",
        name: "gear",
        keywords: "settings config cog",
        category: "Technical",
    },
    SymbolEntry {
        ch: "◉",
        name: "fisheye",
        keywords: "circle dot radio button selected",
        category: "Technical",
    },
    SymbolEntry {
        ch: "○",
        name: "white circle",
        keywords: "circle empty radio button",
        category: "Technical",
    },
    SymbolEntry {
        ch: "●",
        name: "black circle",
        keywords: "circle filled dot",
        category: "Technical",
    },
    SymbolEntry {
        ch: "□",
        name: "white square",
        keywords: "square empty checkbox",
        category: "Technical",
    },
    SymbolEntry {
        ch: "■",
        name: "black square",
        keywords: "square filled block",
        category: "Technical",
    },
    SymbolEntry {
        ch: "◆",
        name: "black diamond",
        keywords: "diamond filled rhombus",
        category: "Technical",
    },
    SymbolEntry {
        ch: "◇",
        name: "white diamond",
        keywords: "diamond empty rhombus",
        category: "Technical",
    },
    SymbolEntry {
        ch: "▲",
        name: "black up triangle",
        keywords: "triangle up arrow",
        category: "Technical",
    },
    SymbolEntry {
        ch: "▼",
        name: "black down triangle",
        keywords: "triangle down arrow",
        category: "Technical",
    },
    SymbolEntry {
        ch: "◀",
        name: "black left triangle",
        keywords: "triangle left arrow previous",
        category: "Technical",
    },
    SymbolEntry {
        ch: "▶",
        name: "black right triangle",
        keywords: "triangle right arrow play next",
        category: "Technical",
    },
    // ── Greek Letters ──
    SymbolEntry {
        ch: "α",
        name: "alpha",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "β",
        name: "beta",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "γ",
        name: "gamma",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "δ",
        name: "delta",
        keywords: "greek letter change",
        category: "Greek",
    },
    SymbolEntry {
        ch: "ε",
        name: "epsilon",
        keywords: "greek letter small",
        category: "Greek",
    },
    SymbolEntry {
        ch: "ζ",
        name: "zeta",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "η",
        name: "eta",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "θ",
        name: "theta",
        keywords: "greek letter angle",
        category: "Greek",
    },
    SymbolEntry {
        ch: "ι",
        name: "iota",
        keywords: "greek letter small tiny",
        category: "Greek",
    },
    SymbolEntry {
        ch: "κ",
        name: "kappa",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "λ",
        name: "lambda",
        keywords: "greek letter function anonymous",
        category: "Greek",
    },
    SymbolEntry {
        ch: "μ",
        name: "mu",
        keywords: "greek letter micro",
        category: "Greek",
    },
    SymbolEntry {
        ch: "ν",
        name: "nu",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "ξ",
        name: "xi",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "π",
        name: "pi",
        keywords: "greek letter circle ratio",
        category: "Greek",
    },
    SymbolEntry {
        ch: "ρ",
        name: "rho",
        keywords: "greek letter density",
        category: "Greek",
    },
    SymbolEntry {
        ch: "σ",
        name: "sigma",
        keywords: "greek letter standard deviation",
        category: "Greek",
    },
    SymbolEntry {
        ch: "τ",
        name: "tau",
        keywords: "greek letter time constant",
        category: "Greek",
    },
    SymbolEntry {
        ch: "υ",
        name: "upsilon",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "φ",
        name: "phi",
        keywords: "greek letter golden ratio",
        category: "Greek",
    },
    SymbolEntry {
        ch: "χ",
        name: "chi",
        keywords: "greek letter",
        category: "Greek",
    },
    SymbolEntry {
        ch: "ψ",
        name: "psi",
        keywords: "greek letter wave",
        category: "Greek",
    },
    SymbolEntry {
        ch: "ω",
        name: "omega",
        keywords: "greek letter ohm",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Α",
        name: "capital alpha",
        keywords: "greek letter uppercase",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Β",
        name: "capital beta",
        keywords: "greek letter uppercase",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Γ",
        name: "capital gamma",
        keywords: "greek letter uppercase",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Δ",
        name: "capital delta",
        keywords: "greek letter uppercase change",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Θ",
        name: "capital theta",
        keywords: "greek letter uppercase angle",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Λ",
        name: "capital lambda",
        keywords: "greek letter uppercase",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Ξ",
        name: "capital xi",
        keywords: "greek letter uppercase",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Π",
        name: "capital pi",
        keywords: "greek letter uppercase product",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Σ",
        name: "capital sigma",
        keywords: "greek letter uppercase sum",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Φ",
        name: "capital phi",
        keywords: "greek letter uppercase",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Ψ",
        name: "capital psi",
        keywords: "greek letter uppercase",
        category: "Greek",
    },
    SymbolEntry {
        ch: "Ω",
        name: "capital omega",
        keywords: "greek letter uppercase ohm",
        category: "Greek",
    },
    // ── Card Suits & Music ──
    SymbolEntry {
        ch: "♠",
        name: "spade suit",
        keywords: "card game spade",
        category: "Misc",
    },
    SymbolEntry {
        ch: "♣",
        name: "club suit",
        keywords: "card game club",
        category: "Misc",
    },
    SymbolEntry {
        ch: "♥",
        name: "heart suit",
        keywords: "card game heart love",
        category: "Misc",
    },
    SymbolEntry {
        ch: "♦",
        name: "diamond suit",
        keywords: "card game diamond",
        category: "Misc",
    },
    SymbolEntry {
        ch: "★",
        name: "black star",
        keywords: "star filled rating",
        category: "Misc",
    },
    SymbolEntry {
        ch: "☆",
        name: "white star",
        keywords: "star empty rating",
        category: "Misc",
    },
    SymbolEntry {
        ch: "♪",
        name: "eighth note",
        keywords: "music note single",
        category: "Misc",
    },
    SymbolEntry {
        ch: "♫",
        name: "beamed eighth notes",
        keywords: "music notes double",
        category: "Misc",
    },
    SymbolEntry {
        ch: "♩",
        name: "quarter note",
        keywords: "music note",
        category: "Misc",
    },
    SymbolEntry {
        ch: "♬",
        name: "beamed sixteenth notes",
        keywords: "music notes",
        category: "Misc",
    },
    // ── Superscripts & Subscripts ──
    SymbolEntry {
        ch: "⁰",
        name: "superscript zero",
        keywords: "power exponent super",
        category: "Math",
    },
    SymbolEntry {
        ch: "¹",
        name: "superscript one",
        keywords: "power exponent super first",
        category: "Math",
    },
    SymbolEntry {
        ch: "²",
        name: "superscript two",
        keywords: "power exponent super squared",
        category: "Math",
    },
    SymbolEntry {
        ch: "³",
        name: "superscript three",
        keywords: "power exponent super cubed",
        category: "Math",
    },
    SymbolEntry {
        ch: "ⁿ",
        name: "superscript n",
        keywords: "power exponent super nth",
        category: "Math",
    },
    SymbolEntry {
        ch: "₀",
        name: "subscript zero",
        keywords: "sub index",
        category: "Math",
    },
    SymbolEntry {
        ch: "₁",
        name: "subscript one",
        keywords: "sub index first",
        category: "Math",
    },
    SymbolEntry {
        ch: "₂",
        name: "subscript two",
        keywords: "sub index second",
        category: "Math",
    },
    SymbolEntry {
        ch: "₃",
        name: "subscript three",
        keywords: "sub index third",
        category: "Math",
    },
    // ── Fractions ──
    SymbolEntry {
        ch: "½",
        name: "one half",
        keywords: "fraction half",
        category: "Math",
    },
    SymbolEntry {
        ch: "⅓",
        name: "one third",
        keywords: "fraction third",
        category: "Math",
    },
    SymbolEntry {
        ch: "¼",
        name: "one quarter",
        keywords: "fraction quarter fourth",
        category: "Math",
    },
    SymbolEntry {
        ch: "⅛",
        name: "one eighth",
        keywords: "fraction eighth",
        category: "Math",
    },
    SymbolEntry {
        ch: "¾",
        name: "three quarters",
        keywords: "fraction three fourth",
        category: "Math",
    },
    SymbolEntry {
        ch: "⅔",
        name: "two thirds",
        keywords: "fraction two third",
        category: "Math",
    },
];

pub struct SymbolHandler;

impl Default for SymbolHandler {
    fn default() -> Self {
        Self
    }
}

impl SymbolHandler {
    pub fn new() -> Self {
        Self
    }

    fn extract_char(label: &str) -> &str {
        label.split_once(' ').map_or(label, |(ch, _)| ch)
    }
}

#[async_trait]
impl ActionHandler for SymbolHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["sym"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "sym"
    }

    fn description(&self) -> &str {
        "Search and copy symbols by name (sym:arrow or sym arrow)"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return Ok(ActionResult::err(
                "Usage: sym:<name> or sym <name>".to_string(),
            ));
        }

        // If args starts with a non-ASCII char, it's a completion label
        let ch = if trimmed.starts_with(|c: char| !c.is_ascii()) {
            Self::extract_char(trimmed)
        } else {
            // Search for best match
            let lower = trimmed.to_lowercase();
            match SYMBOLS
                .iter()
                .find(|s| s.name.contains(&lower) || s.keywords.contains(&lower))
            {
                Some(s) => s.ch,
                None => {
                    return Ok(ActionResult::err(format!("No symbol found for: {trimmed}")));
                }
            }
        };

        match write_to_clipboard(ch) {
            Ok(()) => Ok(ActionResult::ok(
                format!("Copied {ch} to clipboard"),
                OutputType::Status,
            )),
            Err(e) => Ok(ActionResult::err(format!("Clipboard error: {e}"))),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let query = partial.trim();

        // Empty query → popular symbols
        if query.is_empty() {
            return POPULAR_INDICES
                .iter()
                .enumerate()
                .filter_map(|(i, &idx)| {
                    SYMBOLS.get(idx).map(|s| {
                        CompletionItem::new(
                            format!("{} {}", s.ch, s.name),
                            Some("__none__".into()),
                            (POPULAR_INDICES.len() - i) as u16,
                        )
                        .with_run(format!("sym {}", s.ch))
                        .with_description(s.category.to_string())
                    })
                })
                .collect();
        }

        // Fuzzy match against name + keywords
        let mut matcher_guard = MATCHER.lock().unwrap();
        let matcher = matcher_guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));

        let pattern = Atom::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );

        let mut buf = Vec::new();
        let mut results: Vec<(&SymbolEntry, u16)> = Vec::new();

        for sym in SYMBOLS.iter() {
            // Try matching against "name keywords" combined
            let haystack_str = format!("{} {}", sym.name, sym.keywords);
            buf.clear();
            let haystack = Utf32Str::new(&haystack_str, &mut buf);
            if let Some(score) = pattern.score(haystack, matcher) {
                results.push((sym, score));
            }
        }

        results.sort_by(|a, b| b.1.cmp(&a.1));
        results.truncate(20);

        results
            .into_iter()
            .map(|(sym, score)| {
                CompletionItem::new(
                    format!("{} {}", sym.ch, sym.name),
                    Some("__none__".into()),
                    score,
                )
                .with_run(format!("sym {}", sym.ch))
                .with_description(sym.category.to_string())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_symbol_completions_empty() {
        let handler = SymbolHandler::new();
        let results = handler.completions("").await;
        assert_eq!(results.len(), POPULAR_INDICES.len());
    }

    #[tokio::test]
    async fn test_symbol_completions_arrow() {
        let handler = SymbolHandler::new();
        let results = handler.completions("arrow").await;
        assert!(!results.is_empty());
        assert!(results[0].label.contains('→') || results[0].label.contains("arrow"));
    }

    #[tokio::test]
    async fn test_symbol_completions_infinity() {
        let handler = SymbolHandler::new();
        let results = handler.completions("infinity").await;
        assert!(!results.is_empty());
        assert!(results[0].label.contains('∞'));
    }
}
