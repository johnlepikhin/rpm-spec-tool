//! Visitor trait for the `rpm_spec` AST.
//!
//! Modeled on `syn::visit` / `rustc_ast::visit`: every node has a `visit_*`
//! method on the trait (with a default that calls the corresponding
//! free-standing `walk_*`), and `walk_*` recurses into the node's children.
//! Implementors override only the methods they care about; the defaults
//! handle traversal.
//!
//! The trait is fixed to `T = Span`. Lints always need source positions, and
//! making every signature generic over `T` is noise without payoff.

use rpm_spec::ast::{
    BoolDep, BuildCondition, ChangelogEntry, Comment, Conditional, DepAtom, DepExpr, EVR,
    FileEntry, FileTrigger, FilesContent, IncludeDirective, MacroDef, MacroRef, PreambleContent,
    PreambleItem, Scriptlet, Section, ShellBody, Span, SpecFile, SpecItem, TagValue, Text,
    TextBody, TextSegment, Trigger,
};

/// AST walker. Implement the methods you need; defaults handle traversal.
pub trait Visit<'ast> {
    fn visit_spec(&mut self, node: &'ast SpecFile<Span>) {
        walk_spec(self, node)
    }

    fn visit_item(&mut self, node: &'ast SpecItem<Span>) {
        walk_item(self, node)
    }

    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        walk_preamble(self, node)
    }

    fn visit_section(&mut self, node: &'ast Section<Span>) {
        walk_section(self, node)
    }

    fn visit_macro_def(&mut self, node: &'ast MacroDef<Span>) {
        walk_macro_def(self, node)
    }

    fn visit_build_condition(&mut self, node: &'ast BuildCondition<Span>) {
        walk_build_condition(self, node)
    }

    fn visit_include(&mut self, node: &'ast IncludeDirective<Span>) {
        walk_include(self, node)
    }

    fn visit_statement(&mut self, node: &'ast MacroRef) {
        walk_macro_ref(self, node)
    }

    fn visit_comment(&mut self, node: &'ast Comment<Span>) {
        walk_comment(self, node)
    }

    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        walk_top_conditional(self, node)
    }

    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        walk_preamble_conditional(self, node)
    }

    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        walk_files_conditional(self, node)
    }

    fn visit_preamble_content(&mut self, node: &'ast PreambleContent<Span>) {
        walk_preamble_content(self, node)
    }

    fn visit_files_content(&mut self, node: &'ast FilesContent<Span>) {
        walk_files_content(self, node)
    }

    fn visit_file_entry(&mut self, node: &'ast FileEntry<Span>) {
        walk_file_entry(self, node)
    }

    fn visit_changelog_entry(&mut self, node: &'ast ChangelogEntry<Span>) {
        walk_changelog_entry(self, node)
    }

    fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
        walk_scriptlet(self, node)
    }

    fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
        walk_trigger(self, node)
    }

    fn visit_file_trigger(&mut self, node: &'ast FileTrigger<Span>) {
        walk_file_trigger(self, node)
    }

    fn visit_dep_expr(&mut self, node: &'ast DepExpr) {
        walk_dep_expr(self, node)
    }

    fn visit_dep_atom(&mut self, node: &'ast DepAtom) {
        walk_dep_atom(self, node)
    }

    fn visit_bool_dep(&mut self, node: &'ast BoolDep) {
        walk_bool_dep(self, node)
    }

    fn visit_text(&mut self, node: &'ast Text) {
        walk_text(self, node)
    }

    fn visit_text_segment(&mut self, node: &'ast TextSegment) {
        walk_text_segment(self, node)
    }

    fn visit_macro_ref(&mut self, node: &'ast MacroRef) {
        walk_macro_ref(self, node)
    }

    fn visit_shell_body(&mut self, node: &'ast ShellBody) {
        walk_shell_body(self, node)
    }

    fn visit_text_body(&mut self, node: &'ast TextBody) {
        walk_text_body(self, node)
    }
}

// =====================================================================
// Walkers
// =====================================================================

pub fn walk_spec<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a SpecFile<Span>) {
    for item in &node.items {
        v.visit_item(item);
    }
}

pub fn walk_item<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a SpecItem<Span>) {
    match node {
        SpecItem::Preamble(p) => v.visit_preamble(p),
        SpecItem::Section(s) => v.visit_section(s.as_ref()),
        SpecItem::Conditional(c) => v.visit_top_conditional(c),
        SpecItem::MacroDef(m) => v.visit_macro_def(m),
        SpecItem::BuildCondition(b) => v.visit_build_condition(b),
        SpecItem::Include(i) => v.visit_include(i),
        SpecItem::Statement(m) => v.visit_statement(m.as_ref()),
        SpecItem::Comment(c) => v.visit_comment(c),
        SpecItem::Blank => {}
        _ => {}
    }
}

pub fn walk_preamble<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a PreambleItem<Span>) {
    match &node.value {
        TagValue::Text(t) => v.visit_text(t),
        TagValue::Dep(d) => v.visit_dep_expr(d),
        TagValue::ArchList(items) => {
            for t in items {
                v.visit_text(t);
            }
        }
        TagValue::Bool(_) | TagValue::Number(_) => {}
        _ => {}
    }
}

pub fn walk_section<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a Section<Span>) {
    match node {
        Section::Description { body, .. } => v.visit_text_body(body),
        Section::Package { content, .. } => {
            for c in content {
                v.visit_preamble_content(c);
            }
        }
        Section::BuildScript { body, .. } => v.visit_shell_body(body),
        Section::Files {
            file_lists,
            content,
            ..
        } => {
            for t in file_lists {
                v.visit_text(t);
            }
            for c in content {
                v.visit_files_content(c);
            }
        }
        Section::Scriptlet(s) => v.visit_scriptlet(s),
        Section::Trigger(t) => v.visit_trigger(t),
        Section::FileTrigger(t) => v.visit_file_trigger(t),
        Section::Verify { body, .. } => v.visit_shell_body(body),
        Section::Changelog { entries, .. } => {
            for e in entries {
                v.visit_changelog_entry(e);
            }
        }
        Section::SourceList { entries, .. } | Section::PatchList { entries, .. } => {
            for t in entries {
                v.visit_text(t);
            }
        }
        Section::Sepolicy { body, .. } => v.visit_shell_body(body),
        _ => {}
    }
}

pub fn walk_macro_def<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a MacroDef<Span>) {
    v.visit_text(&node.body);
}

pub fn walk_build_condition<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a BuildCondition<Span>) {
    if let Some(default) = &node.default {
        v.visit_text(default);
    }
}

pub fn walk_include<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a IncludeDirective<Span>) {
    v.visit_text(&node.path);
}

pub fn walk_comment<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a Comment<Span>) {
    v.visit_text(&node.text);
}

pub fn walk_top_conditional<'a, V: Visit<'a> + ?Sized>(
    v: &mut V,
    node: &'a Conditional<Span, SpecItem<Span>>,
) {
    for branch in &node.branches {
        for item in &branch.body {
            v.visit_item(item);
        }
    }
    if let Some(els) = &node.otherwise {
        for item in els {
            v.visit_item(item);
        }
    }
}

pub fn walk_preamble_conditional<'a, V: Visit<'a> + ?Sized>(
    v: &mut V,
    node: &'a Conditional<Span, PreambleContent<Span>>,
) {
    for branch in &node.branches {
        for c in &branch.body {
            v.visit_preamble_content(c);
        }
    }
    if let Some(els) = &node.otherwise {
        for c in els {
            v.visit_preamble_content(c);
        }
    }
}

pub fn walk_files_conditional<'a, V: Visit<'a> + ?Sized>(
    v: &mut V,
    node: &'a Conditional<Span, FilesContent<Span>>,
) {
    for branch in &node.branches {
        for c in &branch.body {
            v.visit_files_content(c);
        }
    }
    if let Some(els) = &node.otherwise {
        for c in els {
            v.visit_files_content(c);
        }
    }
}

pub fn walk_preamble_content<'a, V: Visit<'a> + ?Sized>(
    v: &mut V,
    node: &'a PreambleContent<Span>,
) {
    match node {
        PreambleContent::Item(p) => v.visit_preamble(p),
        PreambleContent::Conditional(c) => v.visit_preamble_conditional(c),
        PreambleContent::Comment(c) => v.visit_comment(c),
        PreambleContent::Blank => {}
        _ => {}
    }
}

pub fn walk_files_content<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a FilesContent<Span>) {
    match node {
        FilesContent::Entry(e) => v.visit_file_entry(e),
        FilesContent::Conditional(c) => v.visit_files_conditional(c),
        FilesContent::Comment(c) => v.visit_comment(c),
        FilesContent::Blank => {}
        _ => {}
    }
}

pub fn walk_file_entry<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a FileEntry<Span>) {
    if let Some(path) = &node.path {
        v.visit_text(&path.path);
    }
}

pub fn walk_changelog_entry<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a ChangelogEntry<Span>) {
    v.visit_text(&node.author);
    if let Some(email) = &node.email {
        v.visit_text(email);
    }
    if let Some(version) = &node.version {
        v.visit_text(version);
    }
    for line in &node.body {
        v.visit_text(line);
    }
}

pub fn walk_scriptlet<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a Scriptlet<Span>) {
    if let Some(from) = &node.from_file {
        v.visit_text(from);
    }
    v.visit_shell_body(&node.body);
}

pub fn walk_trigger<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a Trigger<Span>) {
    for cond in &node.conditions {
        v.visit_dep_expr(cond);
    }
    v.visit_shell_body(&node.body);
}

pub fn walk_file_trigger<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a FileTrigger<Span>) {
    for prefix in &node.prefixes {
        v.visit_text(prefix);
    }
    v.visit_shell_body(&node.body);
}

pub fn walk_dep_expr<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a DepExpr) {
    match node {
        DepExpr::Atom(a) => v.visit_dep_atom(a),
        DepExpr::Rich(b) => v.visit_bool_dep(b.as_ref()),
        _ => {}
    }
}

pub fn walk_dep_atom<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a DepAtom) {
    v.visit_text(&node.name);
    if let Some(arch) = &node.arch {
        v.visit_text(arch);
    }
    if let Some(c) = &node.constraint {
        walk_evr(v, &c.evr);
    }
}

fn walk_evr<'a, V: Visit<'a> + ?Sized>(v: &mut V, evr: &'a EVR) {
    v.visit_text(&evr.version);
    if let Some(rel) = &evr.release {
        v.visit_text(rel);
    }
}

pub fn walk_bool_dep<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a BoolDep) {
    match node {
        BoolDep::And(items) | BoolDep::Or(items) | BoolDep::With(items) => {
            for it in items {
                v.visit_dep_expr(it);
            }
        }
        BoolDep::If {
            cond,
            then,
            otherwise,
        }
        | BoolDep::Unless {
            cond,
            then,
            otherwise,
        } => {
            v.visit_dep_expr(cond);
            v.visit_dep_expr(then);
            if let Some(o) = otherwise {
                v.visit_dep_expr(o);
            }
        }
        BoolDep::Without { left, right } => {
            v.visit_dep_expr(left);
            v.visit_dep_expr(right);
        }
        _ => {}
    }
}

pub fn walk_text<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a Text) {
    for seg in &node.segments {
        v.visit_text_segment(seg);
    }
}

pub fn walk_text_segment<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a TextSegment) {
    match node {
        TextSegment::Literal(_) => {}
        TextSegment::Macro(m) => v.visit_macro_ref(m.as_ref()),
        _ => {}
    }
}

pub fn walk_macro_ref<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a MacroRef) {
    for arg in &node.args {
        v.visit_text(arg);
    }
    if let Some(wv) = &node.with_value {
        v.visit_text(wv);
    }
}

pub fn walk_shell_body<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a ShellBody) {
    for line in &node.lines {
        v.visit_text(line);
    }
}

pub fn walk_text_body<'a, V: Visit<'a> + ?Sized>(v: &mut V, node: &'a TextBody) {
    for line in &node.lines {
        v.visit_text(line);
    }
}
