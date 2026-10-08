//! The steps as a graph: the order they can run in, and what each waits on.

use crate::def::FlowDef;
use std::collections::BTreeSet;

/// Each step's name and what it waits on, checked for cycles and unknown names.
/// A step comes after everything it waits on; otherwise they keep the order
/// they are written in.
pub fn order(steps: &[(String, Vec<String>)]) -> Result<Vec<String>, String> {
    sort(steps).map_err(|(_, message)| message)
}

/// [`order`], with the step a problem is at.
pub(crate) fn sort(steps: &[(String, Vec<String>)]) -> Result<Vec<String>, (String, String)> {
    let known = |s: &str| steps.iter().any(|(n, _)| n == s);
    for (name, after) in steps {
        if let Some(a) = after.iter().find(|a| !known(a)) {
            return Err((name.clone(), no_step(name, a)));
        }
    }
    let mut placed: Vec<String> = Vec::new();
    let is_placed = |placed: &[String], s: &str| placed.iter().any(|p| p == s);
    while placed.len() < steps.len() {
        let next = steps
            .iter()
            .find(|(n, after)| !is_placed(&placed, n) && after.iter().all(|a| is_placed(&placed, a)));
        match next {
            Some((n, _)) => placed.push(n.clone()),
            None => {
                let cycle = cycle(steps, &placed);
                return Err((cycle[0].clone(), wait_on_each_other(&cycle)));
            }
        }
    }
    Ok(placed)
}

/// `x` comes after `y`, which is no step.
pub(crate) fn no_step(step: &str, after: &str) -> String {
    format!("`{step}` comes after `{after}`, which is no step")
}

/// A loop among the steps not yet placed, starting from the first written of
/// them. Each of those waits on at least one other, or it would be placed.
fn cycle(steps: &[(String, Vec<String>)], placed: &[String]) -> Vec<String> {
    let left = |s: &str| !placed.iter().any(|p| p == s);
    let after_of = |s: &str| {
        steps
            .iter()
            .find(|(n, _)| n == s)
            .and_then(|(_, after)| after.iter().find(|a| left(a)))
            .expect("a step left over waits on another left over")
    };
    let mut path: Vec<String> = vec![steps.iter().find(|(n, _)| left(n)).expect("one is left").0.clone()];
    loop {
        let next = after_of(path.last().expect("the path is never empty")).clone();
        if let Some(i) = path.iter().position(|p| *p == next) {
            return path.split_off(i);
        }
        path.push(next);
    }
}

/// `` `x` and `y` wait on each other ``, or `` `x` waits on itself ``.
fn wait_on_each_other(cycle: &[String]) -> String {
    let named: Vec<String> = cycle.iter().map(|s| format!("`{s}`")).collect();
    match named.as_slice() {
        [one] => format!("{one} waits on itself"),
        [rest @ .., last] => format!("{} and {last} wait on each other", rest.join(", ")),
        [] => unreachable!("a cycle has a step"),
    }
}

/// Every step `step` comes after, directly or not.
pub fn upstream(def: &FlowDef, step: &str) -> BTreeSet<String> {
    let steps: Vec<(String, Vec<String>)> = def.steps.iter().map(|s| (s.name.clone(), s.after.clone())).collect();
    reach(&steps, step)
}

/// [`upstream`], over names. Names that are no step are kept and not followed.
pub(crate) fn reach(steps: &[(String, Vec<String>)], step: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![step];
    while let Some(s) = todo.pop() {
        for a in steps.iter().filter(|(n, _)| n == s).flat_map(|(_, after)| after) {
            if seen.insert(a.clone()) {
                todo.push(a);
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(v: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        v.iter()
            .map(|(n, after)| (n.to_string(), after.iter().map(|a| a.to_string()).collect()))
            .collect()
    }

    #[test]
    fn fan_in_waits_on_all() {
        let order = order(&[
            ("gchat".into(), vec![]),
            ("notion".into(), vec![]),
            ("brief".into(), vec!["gchat".into(), "notion".into()]),
        ])
        .unwrap();
        assert_eq!(order.last().unwrap(), "brief");
    }

    #[test]
    fn a_step_comes_after_what_it_waits_on_and_otherwise_as_written() {
        let g = graph(&[("late", &["early"]), ("free", &[]), ("early", &[])]);
        assert_eq!(order(&g).unwrap(), ["free", "early", "late"]);
    }

    #[test]
    fn a_cycle_names_its_steps() {
        let g = graph(&[("a", &[]), ("x", &["y"]), ("y", &["x"])]);
        assert_eq!(order(&g).unwrap_err(), "`x` and `y` wait on each other");
        let g = graph(&[("a", &["c"]), ("b", &["a"]), ("c", &["b"])]);
        assert_eq!(order(&g).unwrap_err(), "`a`, `c` and `b` wait on each other");
        let g = graph(&[("a", &["a"])]);
        assert_eq!(order(&g).unwrap_err(), "`a` waits on itself");
    }

    #[test]
    fn a_step_after_no_step_is_refused() {
        let g = graph(&[("a", &["nope"])]);
        assert_eq!(order(&g).unwrap_err(), "`a` comes after `nope`, which is no step");
    }

    #[test]
    fn upstream_is_everything_before_directly_or_not() {
        let g = graph(&[("a", &[]), ("b", &["a"]), ("c", &["b"]), ("d", &[])]);
        assert_eq!(reach(&g, "c"), ["a", "b"].map(String::from).into());
        assert!(reach(&g, "d").is_empty());
    }
}
