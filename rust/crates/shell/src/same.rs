//! Setting a property only when its value really changes. Slint never repaints for a property set to what it already
//! holds, except for lists, which it can only compare by identity: a list set again every second repaints the window every
//! second whether or not a row changed. Everything the shell sets on a timer or in a loop goes through here.

use slint::Model;
use slint_interpreter::{ComponentInstance, Value};

/// Whether two values hold the same thing, looking inside lists and structs.
pub fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Model(x), Value::Model(y)) => {
            x.row_count() == y.row_count() && (0..x.row_count()).all(|i| match (x.row_data(i), y.row_data(i)) {
                (Some(p), Some(q)) => equal(&p, &q),
                (None, None) => true,
                _ => false,
            })
        }
        (Value::Struct(x), Value::Struct(y)) => {
            let (mut xs, mut ys) = (x.iter().collect::<Vec<_>>(), y.iter().collect::<Vec<_>>());
            xs.sort_by(|p, q| p.0.cmp(q.0));
            ys.sort_by(|p, q| p.0.cmp(q.0));
            xs.len() == ys.len() && xs.iter().zip(&ys).all(|(p, q)| p.0 == q.0 && equal(p.1, q.1))
        }
        _ => a == b,
    }
}

/// `set_property`, skipped when the property already holds `value`. Returns whether the value was set (or failed to).
pub fn set(ui: &ComponentInstance, name: &str, value: Value) -> Result<bool, slint_interpreter::SetPropertyError> {
    if ui.get_property(name).is_ok_and(|now| equal(&now, &value)) {
        return Ok(false);
    }
    ui.set_property(name, value).map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{ModelRc, VecModel};
    use slint_interpreter::Struct;

    fn list(rows: &[&str]) -> Value {
        let rows = rows.iter().map(|r| Value::Struct(Struct::from_iter([("text".to_string(), Value::String((*r).into()))]))).collect::<Vec<_>>();
        Value::Model(ModelRc::new(VecModel::from(rows)))
    }

    #[test]
    fn two_lists_with_the_same_rows_are_equal() {
        assert!(equal(&list(&["a", "b"]), &list(&["a", "b"])));
        assert!(equal(&list(&[]), &list(&[])));
    }

    #[test]
    fn a_different_row_or_length_is_not() {
        assert!(!equal(&list(&["a", "b"]), &list(&["a", "c"])));
        assert!(!equal(&list(&["a"]), &list(&["a", "b"])));
    }
}
