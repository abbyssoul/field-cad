//! The catalog document contract's display-name convenience: "Generate a
//! convenient display name from the template name (`"fancy unicorn"`,
//! `"fancy unicorn 1"`, ...), but allow duplicate scene display names."
//!
//! This only avoids the *default* name colliding by accident. Display names
//! are labels, not identifiers — a caller remains free to pass an explicit
//! override straight through to [`crate::InstantiationPlacement`] even if it
//! duplicates an existing one; nothing here, or in instantiation, refuses a
//! duplicate.

use std::collections::HashSet;

/// `template_name` verbatim if no current object already has that exact
/// name, otherwise `"<template_name> 1"`, `"<template_name> 2"`, ... — the
/// first suffix not already in use. `existing_names` is typically every
/// current scene object's name
/// (`WorldSnapshot::objects().values().map(|o| o.name.as_str())`); this
/// function has no `WorldSnapshot`/UI dependency of its own.
pub fn suggest_display_name<'a>(
    template_name: &str,
    existing_names: impl Iterator<Item = &'a str>,
) -> String {
    let existing: HashSet<&str> = existing_names.collect();
    if !existing.contains(template_name) {
        return template_name.to_owned();
    }
    let mut suffix = 1u32;
    loop {
        let candidate = format!("{template_name} {suffix}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_collision_returns_the_name_as_is() {
        assert_eq!(
            suggest_display_name("electron", std::iter::empty()),
            "electron"
        );
        assert_eq!(
            suggest_display_name("electron", ["proton"].into_iter()),
            "electron"
        );
    }

    #[test]
    fn one_collision_appends_one() {
        assert_eq!(
            suggest_display_name("electron", ["electron"].into_iter()),
            "electron 1"
        );
    }

    #[test]
    fn picks_the_smallest_free_suffix_even_with_a_gap() {
        assert_eq!(suggest_display_name("x", ["x", "x 2"].into_iter()), "x 1");
        assert_eq!(
            suggest_display_name("x", ["x", "x 1", "x 2"].into_iter()),
            "x 3"
        );
    }
}
