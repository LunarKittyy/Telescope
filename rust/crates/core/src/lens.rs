//! How a phone camera's lens is named (widgets/lens_panel.py): the same short English name wherever the phone app was built,
//! and the words of it in the language loaded. The Camera card's tiles and Transforms' lens notes both use it.

use crate::i18n::tr_ctx;
use serde_json::Value;

/// Strips Android boilerplate ("Back", "[phys]") from a camera name; "[auto]" marks a multi-lens camera and goes first, since a
/// tile elides from the right and it's what tells the multi-lens camera from the physical lens it's named after.
pub fn shorten_lens_label(raw: &str, compact: bool) -> String {
    let auto = raw.contains(" [auto]");
    let mut label = raw.replace(" [phys]", "").replace(" [auto]", "").replace("Back ", "").replace("Front ", "F/").trim().to_string();
    if compact {
        label = label.replace("Telephoto", "Tele");
    }
    if auto { format!("Auto {label}") } else { label }
}

fn facing_prefix(facing: &str) -> Option<&'static str> {
    match facing {
        "back" => Some(""),
        "front" => Some("F/"),
        "external" => Some("Ext "),
        _ => None,
    }
}

/// The shortened English name of a lens from the phone's camera entry. A phone that sends `facing`, `focalMm` and `lensKind`
/// gets it from those; an older one sends only the English `label`, which is cut down instead.
pub fn lens_label(cam: &Value) -> String {
    let prefix = cam.get("facing").and_then(Value::as_str).and_then(facing_prefix);
    let Some(prefix) = prefix else {
        return shorten_lens_label(cam.get("label").and_then(Value::as_str).unwrap_or(""), true);
    };
    let focal = cam.get("focalMm").and_then(Value::as_i64).filter(|f| *f > 0);
    let mut label = format!("{prefix}{}", focal.map_or_else(|| "?".to_string(), |f| format!("~{f}mm")));
    if cam.get("hasOis") == Some(&Value::Bool(true)) {
        label += " OIS";
    }
    if cam.get("lensKind").and_then(Value::as_str) == Some("auto") { format!("Auto {label}") } else { label }
}

/// A lens name's English words ("Auto", "F/", "Ext", "Tele") in the current language.
fn lens_words(text: &str) -> String {
    let text = text.replace("F/", "F/ ");
    text.split_whitespace().map(|word| match word {
        "Auto" => tr_ctx("lens", "Auto"),
        "F/" => tr_ctx("lens", "Front") + " ",
        "Ext" => tr_ctx("lens", "External"),
        "Tele" => tr_ctx("lens", "Tele"),
        other => other.to_string(),
    }).collect::<Vec<_>>().join(" ")
}

/// A shortened lens label as shown in a sentence, in the current language: "Auto ~24mm OIS".
pub fn lens_name(label: &str) -> String {
    lens_words(label).split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_older_phone_sends_only_the_label() {
        assert_eq!(lens_label(&json!({"label": "Back Telephoto ~85mm OIS [phys]"})), "Tele ~85mm OIS");
        assert_eq!(lens_label(&json!({"label": "Back ~24mm OIS [phys]"})), "~24mm OIS");
        assert_eq!(lens_label(&json!({"label": "Front [auto]"})), "Auto Front");
        assert_eq!(lens_label(&json!({"id": "0"})), "");
    }

    #[test]
    fn a_newer_phone_says_what_the_lens_is() {
        assert_eq!(lens_label(&json!({"facing": "back", "focalMm": 24, "hasOis": true, "lensKind": "auto"})), "Auto ~24mm OIS");
        assert_eq!(lens_label(&json!({"facing": "front", "focalMm": 0})), "F/?");
        assert_eq!(lens_label(&json!({"facing": "external", "focalMm": 35})), "Ext ~35mm");
    }

    #[test]
    fn names_read_in_a_sentence() {
        assert_eq!(lens_name("Auto ~24mm OIS"), "Auto ~24mm OIS");
        assert_eq!(lens_name("F/~26mm"), "Front ~26mm");
        assert_eq!(lens_name("Ext ~35mm"), "External ~35mm");
        assert_eq!(lens_name("Tele ~85mm OIS"), "Tele ~85mm OIS");
    }
}
