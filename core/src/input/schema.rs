//! The fields of several parts as one schema: the protocols of a capture
//! (`proto::View`), the files of one view (`concat::Concat`). Fields of
//! one name are one field when every part gives it the same kind. When two
//! parts give a name different kinds (DLT's `type` is text, a definition's
//! `type` a number), each becomes a field of its own named after its part,
//! `dlt.type` and `sens.type`, and the plain name is none: a query using it
//! is told which to write (review 2026-11-09, R9: the later one was left
//! out without a word, and the two merges were written twice).

use crate::event::{FieldDef, FieldId, FieldKind};
use std::borrow::Cow;

pub(crate) struct SchemaMap {
    pub schema: Vec<FieldDef>,
    /// Per whole field, where parts have it: a part field's name, and the
    /// only part (qualifier) that applies to, for one split by a clash.
    from: Vec<Vec<(String, Option<String>)>>,
}

/// A name split already (`dlt.type` in a capture of two protocols): its
/// plain part. `net.*` and `json.*` aren't splits.
fn split(name: &str) -> Option<&str> {
    name.split_once('.').filter(|(p, _)| !matches!(*p, "net" | "json" | "nfd")).map(|(_, b)| b)
}

impl SchemaMap {
    /// `lead` fields first (`proto` in a mixed layout), then the fields of
    /// `parts` (`(qualifier, schema)`: a lower-case protocol name) in
    /// order, then `tail`.
    pub(crate) fn new(lead: &[FieldDef], parts: &[(String, &[FieldDef])], tail: &[FieldDef]) -> SchemaMap {
        // Names given different kinds, or split inside a part already (a
        // capture of both protocols): split in every part, so DLT's `type`
        // is `dlt.type` in a `.dlt` file next to such a capture too.
        let mut kinds: Vec<(&str, FieldKind)> = Vec::new();
        let mut clash: Vec<&str> = Vec::new();
        for (_, s) in parts {
            for d in s.iter() {
                if let Some(base) = split(&d.name) {
                    if !clash.contains(&base) {
                        clash.push(base);
                    }
                    continue;
                }
                match kinds.iter().find(|(n, _)| *n == d.name) {
                    Some((_, k)) if *k != d.kind && !clash.contains(&d.name.as_ref()) => clash.push(&d.name),
                    Some(_) => {}
                    None => kinds.push((&d.name, d.kind)),
                }
            }
        }
        let mut schema = lead.to_vec();
        let mut from: Vec<Vec<(String, Option<String>)>> =
            lead.iter().map(|d| vec![(d.name.to_string(), None)]).collect();
        for (q, s) in parts {
            for d in s.iter() {
                let (name, source) = if split(&d.name).is_some() {
                    (d.name.to_string(), (d.name.to_string(), None))
                } else if clash.contains(&d.name.as_ref()) {
                    (format!("{q}.{}", d.name), (d.name.to_string(), Some(q.clone())))
                } else {
                    (d.name.to_string(), (d.name.to_string(), None))
                };
                match schema.iter().position(|w| w.name == name) {
                    Some(i) if schema[i].kind == d.kind => {
                        if !from[i].contains(&source) {
                            from[i].push(source);
                        }
                    }
                    Some(_) => {}
                    None => {
                        schema.push(FieldDef { name: Cow::Owned(name), kind: d.kind, costly: d.costly });
                        from.push(vec![source]);
                    }
                }
            }
        }
        for d in tail {
            schema.push(d.clone());
            from.push(vec![(d.name.to_string(), None)]);
        }
        SchemaMap { schema, from }
    }

    /// For each whole field, the field of a part (`qualifier`, `part`)
    /// that is it: same name and kind, and the part's own for a split one.
    pub(crate) fn ids(&self, qualifier: &str, part: &[FieldDef]) -> Vec<Option<FieldId>> {
        self.schema
            .iter()
            .zip(&self.from)
            .map(|(w, sources)| {
                sources
                    .iter()
                    .filter(|(_, only)| only.as_deref().is_none_or(|o| o == qualifier))
                    .find_map(|(base, _)| part.iter().position(|o| o.name == base.as_str() && o.kind == w.kind))
            })
            .collect()
    }
}

/// A part's name as a field prefix: lower case, letters, digits and `_`.
pub(crate) fn qualifier(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &'static str, kind: FieldKind) -> FieldDef {
        FieldDef::new(name, kind, false)
    }

    #[test]
    fn a_name_of_two_kinds_becomes_two_fields() {
        let dlt = [f("apid", FieldKind::Str), f("type", FieldKind::Str), f("payload", FieldKind::Str)];
        let sens = [f("type", FieldKind::U64), f("payload", FieldKind::Str)];
        let parts = [("dlt".to_owned(), &dlt[..]), ("sens".to_owned(), &sens[..])];
        let m = SchemaMap::new(&[f("proto", FieldKind::Name)], &parts, &[f("file", FieldKind::Str)]);
        let names: Vec<&str> = m.schema.iter().map(|d| d.name.as_ref()).collect();
        assert_eq!(names, ["proto", "apid", "dlt.type", "payload", "sens.type", "file"]);
        assert_eq!(m.schema[4].kind, FieldKind::U64);
        assert_eq!(m.ids("dlt", &dlt), [None, Some(0), Some(1), Some(2), None, None]);
        assert_eq!(m.ids("sens", &sens), [None, None, None, Some(1), Some(0), None]);
        // The same kind everywhere: one field, plain.
        let parts = [("dlt".to_owned(), &dlt[..]), ("dlt2".to_owned(), &dlt[..])];
        let m = SchemaMap::new(&[], &parts, &[]);
        assert_eq!(m.schema.len(), 3);
        assert_eq!(m.ids("dlt2", &dlt), [Some(0), Some(1), Some(2)]);
        assert_eq!(qualifier("My-Proto 2"), "my_proto_2");
        // Split in one part already (a capture of both): split in the
        // others too, and one field each.
        let both = [f("dlt.type", FieldKind::Str), f("sens.type", FieldKind::U64), f("net.src", FieldKind::Str)];
        let parts = [("dlt".to_owned(), &dlt[..]), ("c".to_owned(), &both[..])];
        let m = SchemaMap::new(&[], &parts, &[]);
        let names: Vec<&str> = m.schema.iter().map(|d| d.name.as_ref()).collect();
        assert_eq!(names, ["apid", "dlt.type", "payload", "sens.type", "net.src"]);
        assert_eq!(m.ids("dlt", &dlt), [Some(0), Some(1), Some(2), None, None]);
        assert_eq!(m.ids("c", &both), [None, Some(0), None, Some(1), Some(2)]);
    }
}
