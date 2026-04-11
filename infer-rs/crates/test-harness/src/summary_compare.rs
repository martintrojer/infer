// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Compare Pulse summaries between OCaml and Rust at a semantic level.
//!
//! The goal is not raw JSON identity. Instead we parse OCaml's
//! `all_summaries.json` into a Rust/OCaml-neutral canonical model that:
//! - keeps fine-grained `PrePost` state
//! - alpha-renames abstract values
//! - ignores presentation-only ordering noise
//!
//! Step 1 compares `main.pre_post_list`. Specialized summaries are the next
//! layer to add on top of the same canonical model.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::Path;

/// Raw pre/post state before abstract values are alpha-renamed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawPrePost {
    pub kind: String,
    pub pre_stack: Vec<(String, String)>,
    pub post_stack: Vec<(String, String)>,
    pub pre_heap: Vec<RawEdge>,
    pub post_heap: Vec<RawEdge>,
    pub pre_attrs: Vec<(String, Vec<String>)>,
    pub post_attrs: Vec<(String, Vec<String>)>,
    pub conditions: Vec<String>,
    pub phi: Vec<String>,
    pub diagnostic: Option<String>,
}

/// Raw heap edge before alpha-renaming.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct RawEdge {
    pub src: String,
    pub access: String,
    pub dst: String,
}

/// Raw per-procedure summary before canonicalization.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawProcedureSummary {
    pub main: Vec<RawPrePost>,
}

/// Canonical semantic summary used for comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CanonicalProcedureSummary {
    pub main: Vec<CanonicalPrePost>,
}

/// Canonical `PrePost` with alpha-renamed abstract values.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalPrePost {
    pub kind: String,
    pub pre_stack: Vec<String>,
    pub post_stack: Vec<String>,
    pub pre_heap: Vec<String>,
    pub post_heap: Vec<String>,
    pub pre_attrs: Vec<String>,
    pub post_attrs: Vec<String>,
    pub conditions: Vec<String>,
    pub phi: Vec<String>,
    pub diagnostic: Option<String>,
}

impl RawProcedureSummary {
    pub fn canonicalize(&self) -> CanonicalProcedureSummary {
        let mut main: Vec<_> = self.main.iter().map(RawPrePost::canonicalize).collect();
        main.sort();
        CanonicalProcedureSummary { main }
    }
}

impl RawPrePost {
    pub fn canonicalize(&self) -> CanonicalPrePost {
        let mut id_canonicalizer = IdCanonicalizer::new(self);

        let mut pre_stack = self.pre_stack.clone();
        pre_stack.sort();
        for (_, addr) in &pre_stack {
            id_canonicalizer.visit_id(addr);
        }

        let mut post_stack = self.post_stack.clone();
        post_stack.sort();
        for (_, addr) in &post_stack {
            id_canonicalizer.visit_id(addr);
        }

        for (addr, _) in &self.pre_attrs {
            id_canonicalizer.visit_id(addr);
        }
        for (addr, _) in &self.post_attrs {
            id_canonicalizer.visit_id(addr);
        }

        for text in self
            .conditions
            .iter()
            .chain(self.phi.iter())
            .chain(self.diagnostic.iter())
        {
            id_canonicalizer.visit_ids_in_text(text);
        }

        let mut pre_heap: Vec<_> = self
            .pre_heap
            .iter()
            .map(|edge| {
                format!(
                    "{} -{}-> {}",
                    id_canonicalizer.canonical_id(&edge.src),
                    id_canonicalizer.replace_ids(&edge.access),
                    id_canonicalizer.canonical_id(&edge.dst)
                )
            })
            .collect();
        pre_heap.sort();

        let mut post_heap: Vec<_> = self
            .post_heap
            .iter()
            .map(|edge| {
                format!(
                    "{} -{}-> {}",
                    id_canonicalizer.canonical_id(&edge.src),
                    id_canonicalizer.replace_ids(&edge.access),
                    id_canonicalizer.canonical_id(&edge.dst)
                )
            })
            .collect();
        post_heap.sort();

        let pre_stack: Vec<_> = pre_stack
            .into_iter()
            .map(|(var, addr)| format!("{var}={}", id_canonicalizer.canonical_id(&addr)))
            .collect();

        let post_stack: Vec<_> = post_stack
            .into_iter()
            .map(|(var, addr)| format!("{var}={}", id_canonicalizer.canonical_id(&addr)))
            .collect();

        let pre_attrs = canonicalize_attrs(&self.pre_attrs, &id_canonicalizer);
        let post_attrs = canonicalize_attrs(&self.post_attrs, &id_canonicalizer);

        let mut conditions: Vec<_> = self
            .conditions
            .iter()
            .map(|condition| id_canonicalizer.replace_ids(condition))
            .collect();
        conditions.sort();

        let mut phi: Vec<_> = self
            .phi
            .iter()
            .map(|item| id_canonicalizer.replace_ids(item))
            .collect();
        phi = canonicalize_phi_items(&phi);

        CanonicalPrePost {
            kind: self.kind.clone(),
            pre_stack,
            post_stack,
            pre_heap,
            post_heap,
            pre_attrs,
            post_attrs,
            conditions,
            phi,
            diagnostic: self
                .diagnostic
                .as_ref()
                .map(|diagnostic| id_canonicalizer.replace_ids(diagnostic)),
        }
    }
}

fn canonicalize_attrs(
    attrs: &[(String, Vec<String>)],
    id_canonicalizer: &IdCanonicalizer,
) -> Vec<String> {
    let mut result: Vec<_> = attrs
        .iter()
        .map(|(addr, attr_list)| {
            let mut attr_list: Vec<_> = attr_list
                .iter()
                .map(|attr| id_canonicalizer.replace_ids(attr))
                .collect();
            attr_list.sort();
            attr_list.dedup();
            format!(
                "{}:[{}]",
                id_canonicalizer.canonical_id(addr),
                attr_list.join(", ")
            )
        })
        .collect();
    result.sort();
    result
}

fn canonicalize_phi_items(phi: &[String]) -> Vec<String> {
    let eqs: HashMap<_, _> = phi
        .iter()
        .filter_map(|item| {
            let rest = item.strip_prefix("eq:")?;
            let (lhs, rhs) = rest.split_once('=')?;
            Some((lhs.to_string(), rhs.to_string()))
        })
        .collect();
    let witness_equivs = build_witness_equivalences(&eqs);

    let mut normalized = BTreeSet::new();
    for item in phi {
        if let Some(lhs) = parse_positive_witness_eq(item) {
            normalized.insert(format!("atom:0 < {lhs}"));
        } else if let Some(lhs) = parse_nonpositive_witness_eq(item) {
            normalized.insert(format!("atom:{lhs} <= 0"));
        } else if let Some(term) = parse_is_int_item(item) {
            for normalized_term in normalize_is_int_terms(term, &eqs, &witness_equivs) {
                normalized.insert(format!("is_int({normalized_term})"));
            }
        } else {
            normalized.insert(item.clone());
        }
    }

    normalized.into_iter().collect()
}

fn parse_is_int_item(item: &str) -> Option<&str> {
    item.strip_prefix("is_int(")?.strip_suffix(')')
}

fn normalize_is_int_terms(
    term: &str,
    eqs: &HashMap<String, String>,
    witness_equivs: &HashMap<String, String>,
) -> BTreeSet<String> {
    let mut normalized = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    normalize_is_int_term(term, eqs, witness_equivs, &mut visiting, &mut normalized);
    normalized
}

fn normalize_is_int_term(
    term: &str,
    eqs: &HashMap<String, String>,
    witness_equivs: &HashMap<String, String>,
    visiting: &mut BTreeSet<String>,
    normalized: &mut BTreeSet<String>,
) {
    if is_integer_constant(term) || !visiting.insert(term.to_string()) {
        return;
    }

    if let Some(canonical) = witness_equivs.get(term) {
        normalized.insert(canonical.clone());
        visiting.remove(term);
        return;
    }

    if let Some(rhs) = eqs.get(term) {
        normalize_is_int_term(rhs, eqs, witness_equivs, visiting, normalized);
        visiting.remove(term);
        return;
    }

    if let Some(vars) = parse_linear_term_vars(term) {
        normalized.insert(term.to_string());
        if vars.len() == 1 && matches!(vars[0].1, -1 | 1) {
            normalize_is_int_term(&vars[0].0, eqs, witness_equivs, visiting, normalized);
        }
        visiting.remove(term);
        return;
    }

    normalized.insert(term.to_string());
    visiting.remove(term);
}

/// OCaml `PulseArithmetic.solve_lin_ineq` / `PulseFormulaPhi` often encode
/// inequalities through restricted witnesses, for example `x = a + 1`
/// for `0 < x` and `x = -a` for `x <= 0`.
/// Collapse those presentation-only forms so comparator diffs stay focused on
/// real summary semantics rather than solver encoding details.
fn build_witness_equivalences(eqs: &HashMap<String, String>) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for (lhs, rhs) in eqs {
        for witness in [
            parse_positive_witness_rhs(rhs),
            parse_nonpositive_witness_rhs(rhs),
        ]
        .into_iter()
        .flatten()
        {
            result.insert(lhs.clone(), lhs.clone());
            result.insert(rhs.clone(), lhs.clone());
            result.insert(witness, lhs.clone());
        }
    }
    result
}

fn parse_positive_witness_eq(item: &str) -> Option<String> {
    let rest = item.strip_prefix("eq:")?;
    let (lhs, rhs) = rest.split_once('=')?;
    parse_positive_witness_rhs(rhs).map(|_| lhs.to_string())
}

fn parse_positive_witness_rhs(rhs: &str) -> Option<String> {
    let (vars, constant) = parse_linear_term(rhs)?;
    if constant != 1 || vars.len() != 1 || vars[0].1 != 1 {
        return None;
    }
    let witness = &vars[0].0;
    witness.starts_with('a').then(|| witness.clone())
}

fn parse_nonpositive_witness_eq(item: &str) -> Option<String> {
    let rest = item.strip_prefix("eq:")?;
    let (lhs, rhs) = rest.split_once('=')?;
    parse_nonpositive_witness_rhs(rhs).map(|_| lhs.to_string())
}

fn parse_nonpositive_witness_rhs(rhs: &str) -> Option<String> {
    let (vars, constant) = parse_linear_term(rhs)?;
    if constant != 0 || vars.len() != 1 || vars[0].1 != -1 {
        return None;
    }
    let witness = &vars[0].0;
    witness.starts_with('a').then(|| witness.clone())
}

fn parse_linear_term(term: &str) -> Option<(Vec<(String, i32)>, i32)> {
    let inner = term.strip_prefix("lin(")?.strip_suffix(')')?;
    let mut vars = Vec::new();
    let mut constant = 0;
    for part in inner.split(',') {
        if part.is_empty() {
            continue;
        }
        if let Some(value) = part.strip_prefix("const=") {
            constant = value.parse().ok()?;
            continue;
        }
        let (coeff, var) = part.split_once('*')?;
        let coeff = coeff.parse().ok()?;
        vars.push((var.to_string(), coeff));
    }
    Some((vars, constant))
}

fn parse_linear_term_vars(term: &str) -> Option<Vec<(String, i32)>> {
    parse_linear_term(term).map(|(vars, _constant)| vars)
}

fn is_integer_constant(term: &str) -> bool {
    !term.is_empty()
        && term
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'/'))
}

struct IdCanonicalizer {
    mapping: HashMap<String, String>,
    adjacency: HashMap<String, Vec<(String, String)>>,
    preferred_paths: HashMap<String, String>,
    next_unrestricted: usize,
    next_restricted: usize,
}

impl IdCanonicalizer {
    fn new(pre_post: &RawPrePost) -> Self {
        let mut adjacency: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for edge in pre_post.pre_heap.iter().chain(pre_post.post_heap.iter()) {
            adjacency
                .entry(edge.src.clone())
                .or_default()
                .push((edge.access.clone(), edge.dst.clone()));
        }
        for edges in adjacency.values_mut() {
            edges.sort();
        }
        let preferred_paths = build_preferred_paths(pre_post, &adjacency);
        Self {
            mapping: HashMap::new(),
            adjacency,
            preferred_paths,
            next_unrestricted: 1,
            next_restricted: 1,
        }
    }

    fn canonical_id(&self, raw: &str) -> String {
        self.mapping
            .get(raw)
            .cloned()
            .unwrap_or_else(|| raw.to_string())
    }

    fn visit_ids_in_text(&mut self, text: &str) {
        for token in extract_abstract_ids(text) {
            self.visit_id(&token);
        }
    }

    fn visit_id(&mut self, raw: &str) {
        if self.mapping.contains_key(raw) {
            return;
        }

        let canonical = if let Some(path) = self.preferred_paths.get(raw) {
            path.clone()
        } else if raw.starts_with('a') {
            let next = self.next_restricted;
            self.next_restricted += 1;
            format!("a{next}")
        } else {
            let next = self.next_unrestricted;
            self.next_unrestricted += 1;
            format!("v{next}")
        };
        self.mapping.insert(raw.to_string(), canonical);

        if let Some(edges) = self.adjacency.get(raw).cloned() {
            for (access, dst) in edges {
                self.visit_ids_in_text(&access);
                self.visit_id(&dst);
            }
        }
    }

    fn replace_ids(&self, text: &str) -> String {
        replace_abstract_ids(text, &self.mapping)
    }
}

fn build_preferred_paths(
    pre_post: &RawPrePost,
    adjacency: &HashMap<String, Vec<(String, String)>>,
) -> HashMap<String, String> {
    let mut paths = HashMap::new();
    let mut queue = VecDeque::new();

    let mut roots: Vec<_> = pre_post
        .pre_stack
        .iter()
        .chain(pre_post.post_stack.iter())
        .map(|(var, addr)| (var.clone(), addr.clone()))
        .collect();
    roots.sort();

    for (var, addr) in roots {
        if consider_preferred_path(&mut paths, &addr, &var) {
            queue.push_back((addr, var));
        }
    }

    while let Some((raw, path)) = queue.pop_front() {
        if paths.get(&raw) != Some(&path) {
            continue;
        }
        let Some(edges) = adjacency.get(&raw) else {
            continue;
        };
        for (access, dst) in edges {
            let Some(next_path) = extend_path(&path, access) else {
                continue;
            };
            if consider_preferred_path(&mut paths, dst, &next_path) {
                queue.push_back((dst.clone(), next_path));
            }
        }
    }

    paths
}

fn consider_preferred_path(
    paths: &mut HashMap<String, String>,
    raw: &str,
    candidate: &str,
) -> bool {
    match paths.get(raw) {
        Some(existing) if !path_is_better(candidate, existing) => false,
        _ => {
            paths.insert(raw.to_string(), candidate.to_string());
            true
        }
    }
}

fn path_is_better(candidate: &str, existing: &str) -> bool {
    let candidate_key = path_sort_key(candidate);
    let existing_key = path_sort_key(existing);
    candidate_key < existing_key
}

fn path_sort_key(path: &str) -> (usize, usize, &str) {
    let depth = path
        .as_bytes()
        .iter()
        .filter(|byte| matches!(**byte, b'*' | b'.' | b'['))
        .count();
    (depth, path.len(), path)
}

fn extend_path(path: &str, access: &str) -> Option<String> {
    match access {
        "*" => Some(format!("{path}.*")),
        _ if access.starts_with('.') => Some(format!("{path}{access}")),
        _ => None,
    }
}

/// Parse OCaml's `all_summaries.json` into canonical per-procedure summaries.
pub fn parse_ocaml_summaries(path: &Path) -> HashMap<String, CanonicalProcedureSummary> {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let data: serde_json::Value =
        serde_json::from_str(&content).unwrap_or_else(|e| panic!("invalid JSON: {e}"));

    let mut result = HashMap::new();

    let empty = vec![];
    let entries = data.as_array().unwrap_or(&empty);
    for entry in entries {
        let entry = match entry.as_array() {
            Some(a) if a.len() == 2 => a,
            _ => continue,
        };

        let procname = extract_procname(&entry[0]);
        let summaries = match entry[1].as_array() {
            Some(a) => a,
            None => continue,
        };

        for summary_pair in summaries {
            let pair = match summary_pair.as_array() {
                Some(a) if a.len() == 2 => a,
                _ => continue,
            };
            if pair[0].as_str() != Some("pulse") {
                continue;
            }

            let raw = parse_ocaml_pulse_summary(&pair[1]);
            result.insert(procname.clone(), raw.canonicalize());
        }
    }

    result
}

fn parse_ocaml_pulse_summary(value: &serde_json::Value) -> RawProcedureSummary {
    let main = value
        .pointer("/main/pre_post_list")
        .and_then(serde_json::Value::as_array)
        .map(|pre_posts| {
            pre_posts
                .iter()
                .filter_map(parse_ocaml_pre_post)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    RawProcedureSummary { main }
}

fn parse_ocaml_pre_post(value: &serde_json::Value) -> Option<RawPrePost> {
    let (kind, detail) = extract_ocaml_pre_post_kind_and_detail(value)?;
    let state = detail.get("astate").unwrap_or(detail);

    Some(RawPrePost {
        kind,
        pre_stack: parse_ocaml_stack(state.get("pre")),
        post_stack: parse_ocaml_stack(state.get("post")),
        pre_heap: parse_ocaml_heap(state.get("pre")),
        post_heap: parse_ocaml_heap(state.get("post")),
        pre_attrs: parse_ocaml_attrs(state.get("pre")),
        post_attrs: parse_ocaml_attrs(state.get("post")),
        conditions: parse_ocaml_conditions(state.get("path_condition")),
        phi: parse_ocaml_phi(state.get("path_condition").and_then(|pc| pc.get("phi"))),
        diagnostic: detail.get("diagnostic").map(format_ocaml_diagnostic),
    })
}

fn extract_ocaml_pre_post_kind_and_detail(
    value: &serde_json::Value,
) -> Option<(String, &serde_json::Value)> {
    let arr = value.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    if arr[1].is_object() {
        return Some((arr[0].as_str()?.to_string(), &arr[1]));
    }
    let inner = arr[1].as_array()?;
    if inner.len() != 2 || !inner[1].is_object() {
        return None;
    }
    Some((inner[0].as_str()?.to_string(), &inner[1]))
}

fn parse_ocaml_stack(value: Option<&serde_json::Value>) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let Some(stack) = value
        .and_then(|state| state.get("stack"))
        .and_then(serde_json::Value::as_array)
    else {
        return result;
    };

    for entry in stack {
        let Some(entry) = entry.as_array() else {
            continue;
        };
        if entry.len() != 2 {
            continue;
        }
        let Some(var_name) = parse_ocaml_var_name(&entry[0]) else {
            continue;
        };
        let Some(addr) = parse_ocaml_value_id(&entry[1]) else {
            continue;
        };
        result.push((var_name, addr));
    }

    result
}

fn parse_ocaml_heap(value: Option<&serde_json::Value>) -> Vec<RawEdge> {
    let mut result = Vec::new();
    let Some(heap) = value
        .and_then(|state| state.get("heap"))
        .and_then(serde_json::Value::as_array)
    else {
        return result;
    };

    for entry in heap {
        let Some(entry) = entry.as_array() else {
            continue;
        };
        if entry.len() != 2 {
            continue;
        }
        let Some(src) = entry[0].as_str() else {
            continue;
        };
        let Some(edges) = entry[1].as_array() else {
            continue;
        };
        for edge in edges {
            let Some(edge) = edge.as_array() else {
                continue;
            };
            if edge.len() != 2 {
                continue;
            }
            let Some(dst) = parse_ocaml_value_id(&edge[1]) else {
                continue;
            };
            result.push(RawEdge {
                src: src.to_string(),
                access: parse_ocaml_access(&edge[0]),
                dst,
            });
        }
    }

    result
}

fn parse_ocaml_attrs(value: Option<&serde_json::Value>) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    let Some(attrs) = value
        .and_then(|state| state.get("attrs"))
        .and_then(serde_json::Value::as_array)
    else {
        return result;
    };

    for entry in attrs {
        let Some(entry) = entry.as_array() else {
            continue;
        };
        if entry.len() != 2 {
            continue;
        }
        let Some(addr) = entry[0].as_str() else {
            continue;
        };
        let Some(attr_list) = entry[1].as_array() else {
            continue;
        };
        result.push((
            addr.to_string(),
            attr_list.iter().map(format_ocaml_attr).collect(),
        ));
    }

    result
}

fn parse_ocaml_conditions(value: Option<&serde_json::Value>) -> Vec<String> {
    let mut result = Vec::new();
    let Some(conditions) = value
        .and_then(|path_condition| path_condition.get("conditions"))
        .and_then(serde_json::Value::as_array)
    else {
        return result;
    };

    for condition in conditions {
        let Some(condition) = condition.as_array() else {
            continue;
        };
        let Some(atom) = condition.first() else {
            continue;
        };
        result.push(format!("cond:{}", format_ocaml_atom(atom)));
    }

    result
}

fn parse_ocaml_phi(value: Option<&serde_json::Value>) -> Vec<String> {
    let mut result = Vec::new();
    let Some(phi) = value.and_then(serde_json::Value::as_object) else {
        return result;
    };

    if let Some(term_eqs) = phi.get("term_eqs").and_then(serde_json::Value::as_array) {
        for entry in term_eqs {
            let Some(entry) = entry.as_array() else {
                continue;
            };
            if entry.len() != 2 {
                continue;
            }
            let Some(var) = entry[1].as_str() else {
                continue;
            };
            result.push(format!("eq:{var}={}", format_ocaml_term(&entry[0])));
        }
    }

    if let Some(atoms) = phi.get("atoms").and_then(serde_json::Value::as_array) {
        for atom in atoms {
            let atom = format_ocaml_atom(atom);
            if atom.starts_with("is_int(") {
                result.push(atom);
            } else {
                result.push(format!("atom:{atom}"));
            }
        }
    }

    if let Some(intervals) = phi.get("intervals").and_then(serde_json::Value::as_array) {
        for interval in intervals {
            result.push(format!("interval:{}", compact_json(interval)));
        }
    }

    result
}

fn parse_ocaml_var_name(value: &serde_json::Value) -> Option<String> {
    let arr = value.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    let tag = arr[0].as_str()?;
    match tag {
        "ProgramVar" | "Local" | "Pvar" => arr[1]
            .get("plain")
            .and_then(serde_json::Value::as_str)
            .map(normalize_var_name),
        "LogicalVar" | "Ident" => {
            if let Some(obj) = arr[1].as_object() {
                obj.get("name")
                    .and_then(|name| name.get("plain"))
                    .and_then(serde_json::Value::as_str)
                    .map(normalize_var_name)
                    .or_else(|| {
                        obj.get("name")
                            .and_then(serde_json::Value::as_str)
                            .map(normalize_var_name)
                    })
            } else {
                arr[1].as_str().map(normalize_var_name)
            }
        }
        _ => Some(compact_json(value)),
    }
}

fn parse_ocaml_value_id(value: &serde_json::Value) -> Option<String> {
    if let Some(id) = value.as_str() {
        return Some(id.to_string());
    }
    let arr = value.as_array()?;
    if let Some(id) = arr.first().and_then(serde_json::Value::as_str) {
        if looks_like_abstract_id(id) {
            return Some(id.to_string());
        }
    }
    arr.iter()
        .skip(1)
        .filter_map(serde_json::Value::as_str)
        .find(|candidate| looks_like_abstract_id(candidate))
        .map(ToOwned::to_owned)
}

fn looks_like_abstract_id(value: &str) -> bool {
    value.starts_with('v') || value.starts_with('a')
}

fn parse_ocaml_access(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    let Some(tag) = arr.first().and_then(serde_json::Value::as_str) else {
        return compact_json(value);
    };
    match tag {
        "Dereference" => "*".to_string(),
        "FieldAccess" => {
            let field = arr
                .get(1)
                .map(extract_field_name)
                .unwrap_or_else(|| "?".into());
            format!(".{field}")
        }
        "ArrayAccess" => {
            let index = arr
                .get(2)
                .map(format_ocaml_term)
                .unwrap_or_else(|| "?".into());
            format!("[{index}]")
        }
        _ => compact_json(value),
    }
}

fn format_ocaml_attr(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    let Some(tag) = arr.first().and_then(serde_json::Value::as_str) else {
        return compact_json(value);
    };
    match tag {
        "Initialized" => "Initialized".to_string(),
        "WrittenTo" => "WrittenTo".to_string(),
        "MustBeValid" => {
            let reason = arr.get(3).filter(|reason| !reason.is_null());
            if let Some(reason) = reason {
                format!("MustBeValid({})", compact_json(reason))
            } else {
                "MustBeValid".to_string()
            }
        }
        "MustBeInitialized" => "MustBeInitialized".to_string(),
        "Invalid" => arr
            .get(1)
            .map(format_ocaml_invalidation)
            .map(|inv| format!("Invalid({inv})"))
            .unwrap_or_else(|| "Invalid(?)".to_string()),
        "Allocated" => arr
            .get(1)
            .map(format_ocaml_allocator)
            .map(|allocator| format!("Allocated({allocator})"))
            .unwrap_or_else(|| "Allocated(?)".to_string()),
        "Closure" => arr
            .get(1)
            .map(format_ocaml_procname)
            .map(|proc| format!("Closure({proc})"))
            .unwrap_or_else(|| "Closure(?)".to_string()),
        "ReturnedFromUnknown" => {
            let args = arr
                .get(1)
                .and_then(serde_json::Value::as_array)
                .map(|values| values.iter().map(compact_json).collect::<Vec<_>>())
                .unwrap_or_default();
            format!("ReturnedFromUnknown({})", args.join(","))
        }
        "StaticType" => arr
            .get(1)
            .map(compact_json)
            .map(|typ| format!("StaticType({typ})"))
            .unwrap_or_else(|| "StaticType(?)".to_string()),
        "UsedAsBranchCond" => arr
            .get(1)
            .map(format_ocaml_procname)
            .map(|proc| format!("UsedAsBranchCond({proc})"))
            .unwrap_or_else(|| "UsedAsBranchCond(?)".to_string()),
        other => other.to_string(),
    }
}

fn format_ocaml_invalidation(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    let Some(tag) = arr.first().and_then(serde_json::Value::as_str) else {
        return compact_json(value);
    };
    match tag {
        "ConstantDereference" => {
            let constant = arr
                .get(1)
                .map(compact_json)
                .unwrap_or_else(|| "?".to_string());
            format!("ConstantDereference({constant})")
        }
        "ComparedToNullInThisProcedure" => "ComparedToNullInThisProcedure".to_string(),
        "CFree" | "CppDelete" | "CppDeleteArray" | "EndIterator" | "FClose" | "OptionalEmpty" => {
            tag.to_string()
        }
        "GoneOutOfScope" => "GoneOutOfScope".to_string(),
        "StdVector" => {
            let function = arr
                .get(1)
                .map(compact_json)
                .unwrap_or_else(|| "?".to_string());
            format!("StdVector({function})")
        }
        _ => compact_json(value),
    }
}

fn format_ocaml_allocator(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    let Some(tag) = arr.first().and_then(serde_json::Value::as_str) else {
        return compact_json(value);
    };
    match tag {
        "CMalloc" | "CRealloc" | "CppNew" | "CppNewArray" | "HackAsync" => tag.to_string(),
        "CustomMalloc" | "CustomRealloc" | "CustomFree" | "JavaResource" | "CSharpResource" => {
            let proc = arr
                .get(1)
                .map(format_ocaml_procname)
                .unwrap_or_else(|| "?".to_string());
            format!("{tag}({proc})")
        }
        _ => compact_json(value),
    }
}

fn format_ocaml_diagnostic(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    let Some(tag) = arr.first().and_then(serde_json::Value::as_str) else {
        return compact_json(value);
    };
    match tag {
        "AccessToInvalidAddress" => arr
            .get(1)
            .and_then(|value| value.get("invalidation"))
            .map(format_ocaml_invalidation)
            .map(|inv| format!("AccessToInvalidAddress({inv})"))
            .unwrap_or_else(|| "AccessToInvalidAddress(?)".to_string()),
        "MemoryLeak" => "MemoryLeak".to_string(),
        "RetainCycle" => "RetainCycle".to_string(),
        _ => compact_json(value),
    }
}

fn format_ocaml_atom(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    let Some(tag) = arr.first().and_then(serde_json::Value::as_str) else {
        return compact_json(value);
    };
    if arr.len() != 3 {
        return compact_json(value);
    }
    let lhs = &arr[1];
    let rhs = &arr[2];

    if tag == "Equal" {
        if let Some(term) = extract_is_int_term(lhs, rhs) {
            return format!("is_int({term})");
        }
    }

    let lhs = format_ocaml_term(lhs);
    let rhs = format_ocaml_term(rhs);
    match tag {
        "Equal" => format!("{lhs} = {rhs}"),
        "NotEqual" => format!("{lhs} != {rhs}"),
        "LessEqual" => format!("{lhs} <= {rhs}"),
        "LessThan" => format!("{lhs} < {rhs}"),
        _ => compact_json(value),
    }
}

fn extract_is_int_term(lhs: &serde_json::Value, rhs: &serde_json::Value) -> Option<String> {
    if is_const_one(rhs) {
        return parse_is_int_term(lhs);
    }
    if is_const_one(lhs) {
        return parse_is_int_term(rhs);
    }
    None
}

fn parse_is_int_term(value: &serde_json::Value) -> Option<String> {
    let arr = value.as_array()?;
    if arr.first().and_then(serde_json::Value::as_str) != Some("IsInt") {
        return None;
    }
    arr.get(1).map(format_ocaml_term)
}

fn is_const_one(value: &serde_json::Value) -> bool {
    let Some(arr) = value.as_array() else {
        return false;
    };
    if arr.first().and_then(serde_json::Value::as_str) != Some("Const") {
        return false;
    }
    arr.get(1)
        .map(format_ocaml_q)
        .is_some_and(|constant| constant == "1")
}

fn format_ocaml_term(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    let Some(tag) = arr.first().and_then(serde_json::Value::as_str) else {
        return compact_json(value);
    };
    match tag {
        "Var" => arr
            .get(1)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_string(),
        "Const" => arr
            .get(1)
            .map(format_ocaml_q)
            .unwrap_or_else(|| "?".to_string()),
        "Linear" => arr
            .get(1)
            .map(format_ocaml_linear)
            .unwrap_or_else(|| "lin(?)".to_string()),
        "FunctionApplication" => {
            let Some(obj) = arr.get(1).and_then(serde_json::Value::as_object) else {
                return compact_json(value);
            };
            let callee = obj
                .get("f")
                .map(format_ocaml_function_target)
                .unwrap_or_else(|| "?".to_string());
            let actuals = obj
                .get("actuals")
                .and_then(serde_json::Value::as_array)
                .map(|actuals| actuals.iter().map(format_ocaml_term).collect::<Vec<_>>())
                .unwrap_or_default();
            format!("{callee}({})", actuals.join(","))
        }
        "IsInt" => arr
            .get(1)
            .map(format_ocaml_term)
            .map(|term| format!("is_int({term})"))
            .unwrap_or_else(|| "is_int(?)".to_string()),
        _ => compact_json(value),
    }
}

fn format_ocaml_function_target(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    if arr.first().and_then(serde_json::Value::as_str) == Some("Procname") {
        return arr
            .get(1)
            .map(format_ocaml_procname)
            .unwrap_or_else(|| "?".to_string());
    }
    compact_json(value)
}

fn format_ocaml_linear(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    if arr.len() != 2 {
        return compact_json(value);
    }

    let terms = arr[0]
        .as_array()
        .map(|terms| {
            let mut terms = terms
                .iter()
                .filter_map(|term| {
                    let term = term.as_array()?;
                    if term.len() != 2 {
                        return None;
                    }
                    Some((term[0].as_str()?.to_string(), format_ocaml_q(&term[1])))
                })
                .collect::<Vec<_>>();
            terms.sort();
            terms
        })
        .unwrap_or_default();

    format_linear(terms, format_ocaml_q(&arr[1]))
}

fn format_ocaml_q(value: &serde_json::Value) -> String {
    let Some(obj) = value.as_object() else {
        return compact_json(value);
    };
    let num = obj
        .get("num")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let den = obj
        .get("den")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    if den == "1" {
        num.to_string()
    } else {
        format!("{num}/{den}")
    }
}

fn format_linear(mut terms: Vec<(String, String)>, constant: String) -> String {
    terms.sort();
    if terms.is_empty() {
        return constant;
    }
    let mut pieces: Vec<String> = terms
        .into_iter()
        .map(|(var, coeff)| format!("{coeff}*{var}"))
        .collect();
    if constant != "0" {
        pieces.push(format!("const={constant}"));
    }
    format!("lin({})", pieces.join(","))
}

fn format_ocaml_procname(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return compact_json(value);
    };
    if arr.len() != 2 {
        return compact_json(value);
    }
    let Some(tag) = arr[0].as_str() else {
        return compact_json(value);
    };
    match tag {
        "C" => arr[1]
            .get("c_name")
            .and_then(serde_json::Value::as_array)
            .and_then(|names| names.last())
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_string(),
        _ => arr[1]
            .get("method_name")
            .or_else(|| arr[1].get("function_name"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| compact_json(value)),
    }
}

/// Extract procedure name from the JSON procname structure.
fn extract_procname(value: &serde_json::Value) -> String {
    format_ocaml_procname(value)
}

fn normalize_var_name(name: &str) -> String {
    match name {
        "__return" => "return".to_string(),
        other => other.to_string(),
    }
}

fn extract_field_name(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Object(obj) => obj
                    .get("field_name")
                    .or_else(|| obj.get("plain"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                _ => None,
            })
            .next_back()
            .unwrap_or_else(|| compact_json(value)),
        serde_json::Value::Object(obj) => obj
            .get("field_name")
            .or_else(|| obj.get("plain"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| compact_json(value)),
        _ => compact_json(value),
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            let parts = items.iter().map(compact_json).collect::<Vec<_>>();
            format!("[{}]", parts.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut entries = map
                .iter()
                .map(|(key, value)| format!("{key}:{}", compact_json(value)))
                .collect::<Vec<_>>();
            entries.sort();
            format!("{{{}}}", entries.join(","))
        }
    }
}

/// Compare OCaml and Rust canonical summaries, returning a report.
pub fn compare_summaries(
    ocaml: &HashMap<String, CanonicalProcedureSummary>,
    rust: &HashMap<String, CanonicalProcedureSummary>,
) -> ComparisonReport {
    let mut report = ComparisonReport::default();

    let all_procs: BTreeSet<&str> = ocaml
        .keys()
        .chain(rust.keys())
        .map(|s| s.as_str())
        .collect();

    for proc_name in all_procs {
        let o = ocaml.get(proc_name);
        let r = rust.get(proc_name);

        match (o, r) {
            (Some(o), Some(r)) => {
                if o == r {
                    report.matching += 1;
                } else {
                    report.differences.push(ProcDiff {
                        proc_name: proc_name.to_string(),
                        diffs: diff_procedure_summary(o, r),
                    });
                }
            }
            (Some(_), None) => report.ocaml_only.push(proc_name.to_string()),
            (None, Some(_)) => report.rust_only.push(proc_name.to_string()),
            (None, None) => {}
        }
    }

    report
}

fn diff_procedure_summary(
    ocaml: &CanonicalProcedureSummary,
    rust: &CanonicalProcedureSummary,
) -> Vec<String> {
    let mut diffs = Vec::new();
    if ocaml.main.len() != rust.main.len() {
        diffs.push(format!(
            "main pre_post count: ocaml={}, rust={}",
            ocaml.main.len(),
            rust.main.len()
        ));
    }

    for (index, (o, r)) in ocaml.main.iter().zip(rust.main.iter()).enumerate() {
        if o != r {
            diffs.push(format!("main[{index}] {}", diff_pre_post(o, r).join("; ")));
        }
    }

    for extra in ocaml.main.iter().skip(rust.main.len()) {
        diffs.push(format!("main missing in rust: {extra:?}"));
    }
    for extra in rust.main.iter().skip(ocaml.main.len()) {
        diffs.push(format!("main extra in rust: {extra:?}"));
    }

    diffs
}

fn diff_pre_post(ocaml: &CanonicalPrePost, rust: &CanonicalPrePost) -> Vec<String> {
    let mut diffs = Vec::new();

    if ocaml.kind != rust.kind {
        diffs.push(format!("kind ocaml={}, rust={}", ocaml.kind, rust.kind));
    }
    diff_string_list("pre_stack", &ocaml.pre_stack, &rust.pre_stack, &mut diffs);
    diff_string_list(
        "post_stack",
        &ocaml.post_stack,
        &rust.post_stack,
        &mut diffs,
    );
    diff_string_list("pre_heap", &ocaml.pre_heap, &rust.pre_heap, &mut diffs);
    diff_string_list("post_heap", &ocaml.post_heap, &rust.post_heap, &mut diffs);
    diff_string_list("pre_attrs", &ocaml.pre_attrs, &rust.pre_attrs, &mut diffs);
    diff_string_list(
        "post_attrs",
        &ocaml.post_attrs,
        &rust.post_attrs,
        &mut diffs,
    );
    diff_string_list(
        "conditions",
        &ocaml.conditions,
        &rust.conditions,
        &mut diffs,
    );
    diff_string_list("phi", &ocaml.phi, &rust.phi, &mut diffs);

    if ocaml.diagnostic != rust.diagnostic {
        diffs.push(format!(
            "diagnostic ocaml={:?}, rust={:?}",
            ocaml.diagnostic, rust.diagnostic
        ));
    }

    diffs
}

fn diff_string_list(label: &str, ocaml: &[String], rust: &[String], diffs: &mut Vec<String>) {
    if ocaml == rust {
        return;
    }

    let ocaml_set: BTreeSet<_> = ocaml.iter().cloned().collect();
    let rust_set: BTreeSet<_> = rust.iter().cloned().collect();
    let missing: Vec<_> = ocaml_set.difference(&rust_set).cloned().collect();
    let extra: Vec<_> = rust_set.difference(&ocaml_set).cloned().collect();
    diffs.push(format!("{label} missing={missing:?} extra={extra:?}",));
}

/// Result of comparing OCaml and Rust summaries.
#[derive(Default, Debug)]
pub struct ComparisonReport {
    pub matching: usize,
    pub differences: Vec<ProcDiff>,
    pub ocaml_only: Vec<String>,
    pub rust_only: Vec<String>,
}

#[derive(Debug)]
pub struct ProcDiff {
    pub proc_name: String,
    pub diffs: Vec<String>,
}

impl std::fmt::Display for ComparisonReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Matching: {}", self.matching)?;
        if !self.differences.is_empty() {
            writeln!(f, "Differences ({}):", self.differences.len())?;
            for d in &self.differences {
                writeln!(f, "  {}:", d.proc_name)?;
                for diff in &d.diffs {
                    writeln!(f, "    {diff}")?;
                }
            }
        }
        if !self.ocaml_only.is_empty() {
            writeln!(f, "OCaml only ({}):", self.ocaml_only.len())?;
            for proc_name in &self.ocaml_only {
                writeln!(f, "  {proc_name}")?;
            }
        }
        if !self.rust_only.is_empty() {
            writeln!(f, "Rust only ({}):", self.rust_only.len())?;
            for proc_name in &self.rust_only {
                writeln!(f, "  {proc_name}")?;
            }
        }
        Ok(())
    }
}

fn extract_abstract_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let starts_token = matches!(bytes[index], b'v' | b'a')
            && (index == 0 || is_token_boundary(bytes[index - 1]));
        if !starts_token {
            index += 1;
            continue;
        }

        let mut end = index + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }

        let valid_token = end > index + 1 && (end == bytes.len() || is_token_boundary(bytes[end]));
        if valid_token {
            result.push(text[index..end].to_string());
            index = end;
        } else {
            index += 1;
        }
    }

    result
}

fn replace_abstract_ids(text: &str, mapping: &HashMap<String, String>) -> String {
    let bytes = text.as_bytes();
    let mut result = String::with_capacity(text.len());
    let mut index = 0;

    while index < bytes.len() {
        let starts_token = matches!(bytes[index], b'v' | b'a')
            && (index == 0 || is_token_boundary(bytes[index - 1]));
        if !starts_token {
            result.push(bytes[index] as char);
            index += 1;
            continue;
        }

        let mut end = index + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }

        let valid_token = end > index + 1 && (end == bytes.len() || is_token_boundary(bytes[end]));
        if valid_token {
            let token = &text[index..end];
            if let Some(replacement) = mapping.get(token) {
                result.push_str(replacement);
            } else {
                result.push_str(token);
            }
            index = end;
        } else {
            result.push(bytes[index] as char);
            index += 1;
        }
    }

    result
}

fn is_token_boundary(byte: u8) -> bool {
    !(byte as char).is_ascii_alphanumeric() && byte != b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_pre_post_canonicalization_alpha_renames_equivalent_states() {
        let left = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("x".to_string(), "v10".to_string())],
                post_stack: vec![
                    ("return".to_string(), "v12".to_string()),
                    ("x".to_string(), "v10".to_string()),
                ],
                pre_heap: vec![RawEdge {
                    src: "v10".to_string(),
                    access: "*".to_string(),
                    dst: "v11".to_string(),
                }],
                post_heap: vec![
                    RawEdge {
                        src: "v10".to_string(),
                        access: "*".to_string(),
                        dst: "v11".to_string(),
                    },
                    RawEdge {
                        src: "v12".to_string(),
                        access: "*".to_string(),
                        dst: "a2".to_string(),
                    },
                ],
                pre_attrs: vec![("v10".to_string(), vec!["MustBeValid".to_string()])],
                post_attrs: vec![
                    ("v11".to_string(), vec!["Initialized".to_string()]),
                    (
                        "a2".to_string(),
                        vec!["Invalid(ConstantDereference(0))".to_string()],
                    ),
                ],
                conditions: vec!["cond:v11 = 0".to_string()],
                phi: vec!["eq:v12=lin(1*v11,const=1)".to_string()],
                diagnostic: Some("AccessToInvalidAddress(ConstantDereference(0))".to_string()),
            }],
        };

        let right = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("x".to_string(), "v3".to_string())],
                post_stack: vec![
                    ("return".to_string(), "v7".to_string()),
                    ("x".to_string(), "v3".to_string()),
                ],
                pre_heap: vec![RawEdge {
                    src: "v3".to_string(),
                    access: "*".to_string(),
                    dst: "v4".to_string(),
                }],
                post_heap: vec![
                    RawEdge {
                        src: "v3".to_string(),
                        access: "*".to_string(),
                        dst: "v4".to_string(),
                    },
                    RawEdge {
                        src: "v7".to_string(),
                        access: "*".to_string(),
                        dst: "a9".to_string(),
                    },
                ],
                pre_attrs: vec![("v3".to_string(), vec!["MustBeValid".to_string()])],
                post_attrs: vec![
                    ("v4".to_string(), vec!["Initialized".to_string()]),
                    (
                        "a9".to_string(),
                        vec!["Invalid(ConstantDereference(0))".to_string()],
                    ),
                ],
                conditions: vec!["cond:v4 = 0".to_string()],
                phi: vec!["eq:v7=lin(1*v4,const=1)".to_string()],
                diagnostic: Some("AccessToInvalidAddress(ConstantDereference(0))".to_string()),
            }],
        };

        assert_eq!(left.canonicalize(), right.canonicalize());
    }

    #[test]
    fn test_parse_ocaml_summaries() {
        let path = Path::new("/tmp/infer_summary_test/all_summaries.json");
        if !path.exists() {
            eprintln!("skipping: summary file not found");
            return;
        }
        let summaries = parse_ocaml_summaries(path);
        assert!(!summaries.is_empty());
        assert!(summaries.contains_key("return_null"));
        assert!(!summaries["return_null"].main.is_empty());
    }

    #[test]
    fn test_canonicalization_prefers_stack_paths_for_reachable_values() {
        let raw = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("i".to_string(), "v1".to_string())],
                post_stack: vec![
                    ("i".to_string(), "v1".to_string()),
                    ("return".to_string(), "v14".to_string()),
                ],
                pre_heap: vec![RawEdge {
                    src: "v1".to_string(),
                    access: "*".to_string(),
                    dst: "v2".to_string(),
                }],
                post_heap: vec![
                    RawEdge {
                        src: "v1".to_string(),
                        access: "*".to_string(),
                        dst: "v2".to_string(),
                    },
                    RawEdge {
                        src: "v14".to_string(),
                        access: "*".to_string(),
                        dst: "v13".to_string(),
                    },
                ],
                pre_attrs: vec![],
                post_attrs: vec![],
                conditions: vec![],
                phi: vec!["eq:v2=lin(1*v13,const=-1)".to_string()],
                diagnostic: None,
            }],
        };

        let canonical = raw.canonicalize();
        let [pre_post] = canonical.main.as_slice() else {
            panic!("expected one pre/post");
        };

        assert_eq!(pre_post.pre_stack, vec!["i=i"]);
        assert_eq!(pre_post.post_stack, vec!["i=i", "return=return"]);
        assert_eq!(pre_post.pre_heap, vec!["i -*-> i.*"]);
        assert_eq!(
            pre_post.post_heap,
            vec!["i -*-> i.*", "return -*-> return.*"]
        );
        assert_eq!(pre_post.phi, vec!["eq:i.*=lin(1*return.*,const=-1)"]);
    }

    #[test]
    fn test_canonicalization_matches_alias_wrapper_abort_shape() {
        let ocaml = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "AbortProgram".to_string(),
                pre_stack: vec![("x".to_string(), "v1".to_string())],
                post_stack: vec![("x".to_string(), "v1".to_string())],
                pre_heap: vec![RawEdge {
                    src: "v1".to_string(),
                    access: "*".to_string(),
                    dst: "v3".to_string(),
                }],
                post_heap: vec![
                    RawEdge {
                        src: "v1".to_string(),
                        access: "*".to_string(),
                        dst: "v3".to_string(),
                    },
                    RawEdge {
                        src: "v3".to_string(),
                        access: "*".to_string(),
                        dst: "v10".to_string(),
                    },
                ],
                pre_attrs: vec![
                    (
                        "v1".to_string(),
                        vec!["MustBeInitialized".to_string(), "MustBeValid".to_string()],
                    ),
                    ("v3".to_string(), vec!["MustBeValid".to_string()]),
                ],
                post_attrs: vec![
                    (
                        "v3".to_string(),
                        vec!["Initialized".to_string(), "WrittenTo".to_string()],
                    ),
                    (
                        "v10".to_string(),
                        vec!["Invalid(ConstantDereference(1))".to_string()],
                    ),
                ],
                conditions: vec![],
                phi: vec!["eq:v10=1".to_string()],
                diagnostic: Some("AccessToInvalidAddress(ConstantDereference(0))".to_string()),
            }],
        };

        let rust = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "AbortProgram".to_string(),
                pre_stack: vec![("x".to_string(), "v1".to_string())],
                post_stack: vec![("x".to_string(), "v1".to_string())],
                pre_heap: vec![RawEdge {
                    src: "v1".to_string(),
                    access: "*".to_string(),
                    dst: "v2".to_string(),
                }],
                post_heap: vec![
                    RawEdge {
                        src: "v1".to_string(),
                        access: "*".to_string(),
                        dst: "v2".to_string(),
                    },
                    RawEdge {
                        src: "v2".to_string(),
                        access: "*".to_string(),
                        dst: "v3".to_string(),
                    },
                ],
                pre_attrs: vec![
                    (
                        "v1".to_string(),
                        vec!["MustBeInitialized".to_string(), "MustBeValid".to_string()],
                    ),
                    ("v2".to_string(), vec!["MustBeValid".to_string()]),
                ],
                post_attrs: vec![
                    (
                        "v2".to_string(),
                        vec!["Initialized".to_string(), "WrittenTo".to_string()],
                    ),
                    (
                        "v3".to_string(),
                        vec!["Invalid(ConstantDereference(1))".to_string()],
                    ),
                ],
                conditions: vec![],
                phi: vec!["eq:v3=1".to_string()],
                diagnostic: Some("AccessToInvalidAddress(ConstantDereference(0))".to_string()),
            }],
        };

        assert_eq!(ocaml.canonicalize(), rust.canonicalize());
    }

    #[test]
    fn test_parse_ocaml_abort_wrapper_shape() {
        let value: serde_json::Value = serde_json::json!([
            "Stopped",
            [
                "AbortProgram",
                {
                    "astate": {
                        "post": {
                            "heap": [
                                ["v1", [[["Dereference"], ["v3", "_"]]]],
                                ["v3", [[["Dereference"], ["v10", "_"]]]]
                            ],
                            "stack": [
                                [["ProgramVar", {"plain": "x", "mangled": "x{0}"}], ["Unknown", "v1", "_"]]
                            ],
                            "attrs": [
                                ["v3", [["Initialized"], ["WrittenTo", 3, ["Immediate", {"location": {"file": ["Absolute", "/tmp/specialization.c"], "line": 97, "col": 3, "macro_file_opt": null, "macro_line": -1}, "history": "_"}]]]],
                                ["v10", [["Invalid", ["ConstantDereference", "1"], ["Immediate", {"location": {"file": ["Absolute", "/tmp/specialization.c"], "line": 97, "col": 3, "macro_file_opt": null, "macro_line": -1}, "history": "_"}]]]]
                            ]
                        },
                        "pre": {
                            "heap": [
                                ["v1", [[["Dereference"], ["v3", "_"]]]]
                            ],
                            "stack": [
                                [["ProgramVar", {"plain": "x", "mangled": "x{0}"}], ["Unknown", "v1", "_"]]
                            ],
                            "attrs": [
                                ["v1", [["MustBeInitialized", 0, ["Immediate", {"location": {"file": ["Absolute", "/tmp/specialization.c"], "line": 101, "col": 1, "macro_file_opt": null, "macro_line": -1}, "history": "_"}]], ["MustBeValid", 0, ["Immediate", {"location": {"file": ["Absolute", "/tmp/specialization.c"], "line": 101, "col": 1, "macro_file_opt": null, "macro_line": -1}, "history": "_"}], null]]],
                                ["v3", [["MustBeValid", 3, ["ViaCall", {"f": ["Call", ["C", {"c_name": ["test_alias"], "c_mangled": null, "c_template_args": ["NoTemplate"]}]], "location": {"file": ["Absolute", "/tmp/specialization.c"], "line": 102, "col": 7, "macro_file_opt": null, "macro_line": -1}, "history": "_", "in_call": ["Immediate", {"location": {"file": ["Absolute", "/tmp/specialization.c"], "line": 96, "col": 3, "macro_file_opt": null, "macro_line": -1}, "history": "_"}]}], null]]]
                            ]
                        },
                        "path_condition": {
                            "conditions": [],
                            "phi": {
                                "term_eqs": [
                                    [["Const", {"num": "1", "den": "1"}], "v10"]
                                ],
                                "atoms": []
                            }
                        }
                    },
                    "diagnostic": [
                        "AccessToInvalidAddress",
                        {
                            "invalidation": ["ConstantDereference", "0"]
                        }
                    ]
                }
            ]
        ]);

        let parsed = parse_ocaml_pre_post(&value).expect("abort wrapper should parse");
        let expected = RawPrePost {
            kind: "AbortProgram".to_string(),
            pre_stack: vec![("x".to_string(), "v1".to_string())],
            post_stack: vec![("x".to_string(), "v1".to_string())],
            pre_heap: vec![RawEdge {
                src: "v1".to_string(),
                access: "*".to_string(),
                dst: "v3".to_string(),
            }],
            post_heap: vec![
                RawEdge {
                    src: "v1".to_string(),
                    access: "*".to_string(),
                    dst: "v3".to_string(),
                },
                RawEdge {
                    src: "v3".to_string(),
                    access: "*".to_string(),
                    dst: "v10".to_string(),
                },
            ],
            pre_attrs: vec![
                (
                    "v1".to_string(),
                    vec!["MustBeInitialized".to_string(), "MustBeValid".to_string()],
                ),
                ("v3".to_string(), vec!["MustBeValid".to_string()]),
            ],
            post_attrs: vec![
                (
                    "v3".to_string(),
                    vec!["Initialized".to_string(), "WrittenTo".to_string()],
                ),
                (
                    "v10".to_string(),
                    vec!["Invalid(ConstantDereference(1))".to_string()],
                ),
            ],
            conditions: vec![],
            phi: vec!["eq:v10=1".to_string()],
            diagnostic: Some("AccessToInvalidAddress(ConstantDereference(0))".to_string()),
        };

        assert_eq!(parsed, expected);
    }

    #[test]
    fn test_phi_normalization_resolves_is_int_through_equalities() {
        let left = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("i".to_string(), "v1".to_string())],
                post_stack: vec![("i".to_string(), "v1".to_string())],
                pre_heap: vec![],
                post_heap: vec![],
                pre_attrs: vec![],
                post_attrs: vec![],
                conditions: vec![],
                phi: vec![
                    "eq:v1=lin(1*v2,const=-1)".to_string(),
                    "is_int(v1)".to_string(),
                    "eq:v3=1".to_string(),
                    "is_int(v3)".to_string(),
                ],
                diagnostic: None,
            }],
        };
        let right = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("i".to_string(), "v10".to_string())],
                post_stack: vec![("i".to_string(), "v10".to_string())],
                pre_heap: vec![],
                post_heap: vec![],
                pre_attrs: vec![],
                post_attrs: vec![],
                conditions: vec![],
                phi: vec![
                    "eq:v10=lin(1*v11,const=-1)".to_string(),
                    "is_int(v11)".to_string(),
                    "is_int(lin(1*v11,const=-1))".to_string(),
                    "eq:v12=1".to_string(),
                ],
                diagnostic: None,
            }],
        };

        assert_eq!(left.canonicalize(), right.canonicalize());
    }

    #[test]
    fn test_phi_normalization_collapses_positivity_witness_equalities() {
        let left = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("x".to_string(), "v1".to_string())],
                post_stack: vec![("x".to_string(), "v1".to_string())],
                pre_heap: vec![],
                post_heap: vec![],
                pre_attrs: vec![],
                post_attrs: vec![],
                conditions: vec![],
                phi: vec![
                    "eq:v1=lin(1*a1,const=1)".to_string(),
                    "is_int(a1)".to_string(),
                    "is_int(lin(1*a1,const=1))".to_string(),
                ],
                diagnostic: None,
            }],
        };
        let right = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("x".to_string(), "v10".to_string())],
                post_stack: vec![("x".to_string(), "v10".to_string())],
                pre_heap: vec![],
                post_heap: vec![],
                pre_attrs: vec![],
                post_attrs: vec![],
                conditions: vec![],
                phi: vec!["atom:0 < v10".to_string(), "is_int(v10)".to_string()],
                diagnostic: None,
            }],
        };

        assert_eq!(left.canonicalize(), right.canonicalize());
    }

    #[test]
    fn test_phi_normalization_collapses_nonpositive_witness_equalities() {
        let left = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("x".to_string(), "v1".to_string())],
                post_stack: vec![("x".to_string(), "v1".to_string())],
                pre_heap: vec![],
                post_heap: vec![],
                pre_attrs: vec![],
                post_attrs: vec![],
                conditions: vec![],
                phi: vec![
                    "eq:v1=lin(-1*a1)".to_string(),
                    "is_int(a1)".to_string(),
                    "is_int(lin(-1*a1))".to_string(),
                ],
                diagnostic: None,
            }],
        };
        let right = RawProcedureSummary {
            main: vec![RawPrePost {
                kind: "ContinueProgram".to_string(),
                pre_stack: vec![("x".to_string(), "v10".to_string())],
                post_stack: vec![("x".to_string(), "v10".to_string())],
                pre_heap: vec![],
                post_heap: vec![],
                pre_attrs: vec![],
                post_attrs: vec![],
                conditions: vec![],
                phi: vec!["atom:v10 <= 0".to_string(), "is_int(v10)".to_string()],
                diagnostic: None,
            }],
        };

        assert_eq!(left.canonicalize(), right.canonicalize());
    }
}
