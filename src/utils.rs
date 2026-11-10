use std::path::PathBuf;

pub const REGION_MAP: &[(&str, &str)] = &[
    ("EUW", "euw"),
    ("EUNE", "eune"),
    ("NA", "na"),
    ("KR", "kr"),
    ("BR", "br"),
    ("JP", "jp"),
    ("OCE", "oce"),
    ("RU", "ru"),
    ("TR", "tr"),
    ("LAN", "lan"),
    ("LAS", "las"),
    ("ME", "me"),
    ("SEA", "sea"),
    ("TW", "tw"),
    ("VN", "vn"),
];

pub const TIER_ORDER: &[&str] = &[
    "Challenger",
    "Grandmaster",
    "Master",
    "Diamond",
    "Emerald",
    "Platinum",
    "Gold",
    "Silver",
    "Bronze",
    "Iron",
    "Unranked",
    "Error",
];

pub fn region_from_display(display: &str) -> Option<&'static str> {
    REGION_MAP
        .iter()
        .find(|(label, _)| *label == display)
        .map(|(_, value)| *value)
}

/// Split one bulk line into `(account, name, password)`.
///
/// `--` separates fields. Two fields are `account--password` — no Riot ID yet,
/// which is what a bulk import usually has — and three are
/// `account--name--password`.
///
/// The separator is the literal `--`, not a *run* of dashes, so a password
/// that starts with one dash survives: `user---abc` is `user` / `-abc`. A
/// password that starts with `--` cannot be written at all, because those two
/// dashes read as a field boundary and leave an empty field. Every empty field
/// is rejected, so such a password fails to import instead of silently losing
/// its dashes. Keep it that way: trimming dashes "to be forgiving" is exactly
/// how a password gets corrupted without anyone noticing.
pub fn parse_account_line(line: &str) -> Option<(&str, &str, &str)> {
    let parts: Vec<&str> = line.split("--").map(str::trim).collect();
    let (account_id, name, password) = match parts[..] {
        // Two fields: no Riot ID column at all.
        [account_id, password] => (account_id, "", password),
        // Three fields: the middle one must actually carry a name. An empty
        // middle field means the line had a stray separator (`user----secret`),
        // and guessing which side lost a character is worse than refusing it.
        //
        // The middle field must look like a Riot ID (`Name#TAG`, no leading
        // dash). Otherwise a two-field line whose password contains `--`
        // (`user--pa--ss`) or a line in the old `---` separator form
        // (`user---Name#TAG---pass`) would parse as a different account with
        // a truncated password instead of being rejected.
        [account_id, name, password] if name.contains('#') && !name.starts_with('-') => {
            (account_id, name, password)
        }
        _ => return None,
    };
    (!account_id.is_empty() && !password.is_empty()).then_some((account_id, name, password))
}

pub fn region_display(region: &str) -> String {
    REGION_MAP
        .iter()
        .find(|(_, value)| *value == region)
        .map(|(label, _)| (*label).to_owned())
        .unwrap_or_else(|| region.to_owned())
}

/// The same ordering used by the original application: highest tier first,
/// then division I through IV, then descending LP.
pub fn rank_sort_key(tier: &str, division: &str, lp: &str) -> (usize, usize, i32) {
    let tier_index = TIER_ORDER
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(tier))
        .unwrap_or(TIER_ORDER.len());
    let division_upper = division.trim().to_ascii_uppercase();
    let division_index = match division_upper.as_str() {
        "I" | "1" => 0,
        "II" | "2" => 1,
        "III" | "3" => 2,
        "IV" | "4" => 3,
        _ => 9,
    };
    let lp_value = lp.trim().parse::<i32>().unwrap_or(0);
    (tier_index, division_index, -lp_value)
}

pub fn sort_accounts(accounts: &mut [crate::models::Account]) {
    accounts.sort_by_key(|account| rank_sort_key(&account.tier, &account.division, &account.lp));
}

pub fn app_data_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "home directory is unavailable",
            )
        })?;
    let folder = base.join("LeagueAccounts");
    std::fs::create_dir_all(&folder)?;
    Ok(folder)
}

pub fn accounts_file() -> std::io::Result<PathBuf> {
    Ok(app_data_dir()?.join("league_accounts.json"))
}

pub fn settings_file() -> std::io::Result<PathBuf> {
    Ok(app_data_dir()?.join("settings.json"))
}
