//! Spanish wording for values that come from data: numbers, dates and the
//! names GitHub and the store use for events and states.

use jiff::civil::{Date, Weekday};

/// Decimal number with a comma, as written in Spanish.
pub fn dec(value: f64, places: usize) -> String {
    format!("{value:.places$}").replace('.', ",")
}

/// Whole number with a point every three digits.
pub fn num(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
}

pub fn weekday(date: Date) -> &'static str {
    match date.weekday() {
        Weekday::Monday => "lun",
        Weekday::Tuesday => "mar",
        Weekday::Wednesday => "mié",
        Weekday::Thursday => "jue",
        Weekday::Friday => "vie",
        Weekday::Saturday => "sáb",
        Weekday::Sunday => "dom",
    }
}

/// Day and short month, like "14 sep".
pub fn day_month(date: Date) -> String {
    const MONTHS: [&str; 12] = [
        "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic",
    ];
    format!("{} {}", date.day(), MONTHS[date.month() as usize - 1])
}

/// The metric named in an evidence link.
pub fn metric(key: &str) -> String {
    match key {
        "commits" => "commits".into(),
        "new_work" => "trabajo nuevo".into(),
        "legacy_refactor" => "refactor de legacy".into(),
        "help_others" => "ayuda a otros".into(),
        "churn" => "churn".into(),
        "impact" => "impacto".into(),
        other => other.replace('_', " "),
    }
}

pub fn event(kind: &str) -> String {
    match kind {
        "commit" => "commit".into(),
        "review" => "revisión".into(),
        "comment" => "comentario".into(),
        "review_requested" => "revisión solicitada".into(),
        "ready_for_review" => "listo para revisión".into(),
        "convert_to_draft" => "vuelto a borrador".into(),
        "force_pushed" => "force push".into(),
        "merged" => "mergeado".into(),
        "closed" => "cerrado".into(),
        other => other.replace('_', " "),
    }
}

pub fn review_state(state: &str) -> String {
    match state {
        "APPROVED" => "aprobado".into(),
        "CHANGES_REQUESTED" => "cambios solicitados".into(),
        "COMMENTED" => "comentado".into(),
        "DISMISSED" => "descartado".into(),
        "PENDING" => "pendiente".into(),
        other => other.to_lowercase().replace('_', " "),
    }
}

pub fn identity_status(status: &str) -> &str {
    match status {
        "developer" => "persona",
        "unlinked" => "sin vincular",
        "excluded: bot" => "excluido: bot",
        "unresolved" => "sin resolver",
        other => other,
    }
}
