//! Test-only source with source-supplied fields and levels, standing in
//! for decoded formats such as DLT.

use super::LogSource;
use crate::LineNo;
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) const SCHEMA: &[FieldDef] =
    &[FieldDef::new("apid", FieldKind::Str, false), FieldDef::new("mcnt", FieldKind::U64, false)];

pub(crate) struct Row {
    pub text: &'static str,
    pub apid: Option<&'static str>,
    pub mcnt: Option<u64>,
    pub level: Option<Level>,
}

pub(crate) struct MemSource {
    pub rows: Vec<Row>,
    /// How often a line was rendered, to prove header-only filters skip it.
    pub renders: AtomicUsize,
}

impl MemSource {
    pub fn new(rows: Vec<Row>) -> Self {
        Self { rows, renders: AtomicUsize::new(0) }
    }
}

impl LogSource for MemSource {
    fn name(&self) -> &str {
        "mem"
    }

    fn line_count(&self) -> LineNo {
        self.rows.len() as LineNo
    }

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        self.renders.fetch_add(1, Ordering::Relaxed);
        self.rows.get(line as usize).map(|r| Cow::Owned(r.text.as_bytes().to_vec()))
    }

    fn schema(&self) -> &[FieldDef] {
        SCHEMA
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        let r = self.rows.get(line as usize)?;
        match id {
            0 => r.apid.map(|a| FieldValue::Str(a.as_bytes().into())),
            1 => r.mcnt.map(FieldValue::U64),
            _ => None,
        }
    }

    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        self.rows.get(line as usize).map(|r| r.level)
    }
}
