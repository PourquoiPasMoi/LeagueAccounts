//! Readable names for Riot's queues, used in recording names and the
//! Recordings page ("ARAM: Mayhem", not the internal mode name "KIWI").
//! Queue ids from the League client's `/lol-game-queues/v1/queues`.

/// The name of a queue by id, else by game mode; `None` when unknown (the
/// client's own queue description is used then).
pub fn label(queue_id: i64, game_mode: &str) -> Option<&'static str> {
    let by_id = match queue_id {
        400 => "Normal Draft",
        430 => "Normal Blind",
        420 => "Ranked Solo/Duo",
        440 => "Ranked Flex",
        710 => "Ranked 5s",
        480 => "Swiftplay",
        490 => "Quickplay",
        450 => "ARAM",
        2400 | 3270 => "ARAM: Mayhem",
        700 => "Clash",
        720 => "ARAM Clash",
        740 | 741 => "ARURF Clash",
        900 => "ARURF",
        1010 => "Snow ARURF",
        1900 => "URF",
        1020 => "One for All",
        1300 => "Nexus Blitz",
        1400 => "Ultimate Spellbook",
        1700 | 1710 => "Arena",
        1750 => "Arena 3x6",
        1810..=1840 => "Swarm",
        4310 | 4320 => "Classic Rift",
        3260 | 3262 => "Classic Rift Custom",
        830 | 840 | 843 | 850 | 870 | 880 | 890 => "Co-op vs AI",
        2000 | 2010 | 2020 => "Tutorial",
        3140 => "Practice Tool",
        3200..=3230 => "ARAM Custom",
        0 | 3100..=3130 => "Custom",
        _ => "",
    };
    if !by_id.is_empty() {
        return Some(by_id);
    }
    let by_mode = match game_mode.to_ascii_uppercase().as_str() {
        "PRACTICETOOL" => "Practice Tool",
        "KIWI" => "ARAM: Mayhem",
        "CHERRY" => "Arena",
        "JADE" => "Classic Rift",
        "SWIFTPLAY" => "Swiftplay",
        "ARAM" => "ARAM",
        "URF" | "ARURF" => "URF",
        "ONEFORALL" => "One for All",
        "ULTBOOK" => "Ultimate Spellbook",
        "NEXUSBLITZ" => "Nexus Blitz",
        "STRAWBERRY" => "Swarm",
        "BRAWL" => "Brawl",
        "CLASSIC" => "Classic",
        mode if mode.starts_with("TUTORIAL") => "Tutorial",
        _ => "",
    };
    (!by_mode.is_empty()).then_some(by_mode)
}

/// Mode groups for the recording rules (Settings › Game modes), most important
/// first: the default order of the disk-limit priority.
pub const GROUPS: [&str; 10] = ["ranked_solo", "ranked_flex", "clash", "normal", "arena", "aram", "aram_mayhem", "rotating", "custom", "other"];

/// The mode group of a recording from its queue id and its readable queue name
/// (the name alone works for older recordings without an id).
pub fn group(queue_id: i64, queue: &str) -> &'static str {
    let named = if queue_id > 0 { label(queue_id, "") } else { None };
    match named.unwrap_or(queue) {
        "Ranked Solo/Duo" => "ranked_solo",
        "Ranked Flex" | "Ranked 5s" => "ranked_flex",
        "Clash" => "clash",
        "Normal Draft" | "Normal Blind" | "Swiftplay" | "Quickplay" | "Classic Rift" | "Classic" => "normal",
        "Arena" | "Arena 3x6" => "arena",
        "ARAM" | "ARAM Clash" => "aram",
        "ARAM: Mayhem" => "aram_mayhem",
        "URF" | "ARURF" | "Snow ARURF" | "ARURF Clash" | "One for All" | "Nexus Blitz" | "Ultimate Spellbook" | "Swarm" | "Brawl" => "rotating",
        "Custom" | "Classic Rift Custom" | "ARAM Custom" | "Practice Tool" | "Tutorial" | "Co-op vs AI" => "custom",
        _ => "other",
    }
}

