//! A tab's signals collected together (review 2026-11-14, P3): signals
//! whose query requires a field to have one value (`mid == 11 AND …`, every
//! FIBEX argument the Signals panel adds; `apid == NAV AND /…/`) share one
//! pass over a file, keyed by that field: each row reads the field once and
//! only the signals that want its value look further. The rest collect
//! their own, as [`SignalDef::series`] does. The events are the same either
//! way (tested below); they go into each definition's cache.

use super::*;
use crate::dsl::CmpOp;
use crate::engine::search::LINES_PER_TASK;
use crate::event::FieldId;
use std::collections::HashMap;
use std::ops::Range;

/// The value a query requires a field to have.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Want {
    Num(u64),
    Str(Vec<u8>),
    /// Without case: the value lower case.
    NoCase(Vec<u8>),
}

/// The `FIELD == VALUE` a bound query requires (a condition of its
/// top-level AND, or the whole query); a number first.
fn required(expr: &Expr) -> Option<(FieldId, Want)> {
    let conds: Vec<&Expr> = match expr {
        Expr::And(xs) => xs.iter().collect(),
        e => vec![e],
    };
    let mut text = None;
    for c in conds {
        let Expr::Cond(Cond::Field(f)) = c else { continue };
        let Some(id) = f.id else { continue };
        match &f.test {
            FieldTest::Num(CmpOp::Eq, v) => return Some((id, Want::Num(*v))),
            FieldTest::Str(CmpOp::Eq, v) if text.is_none() => text = Some((id, Want::Str(v.clone()))),
            FieldTest::StrNoCase(CmpOp::Eq, v) if text.is_none() => text = Some((id, Want::NoCase(v.clone()))),
            _ => {}
        }
    }
    text
}

/// The signals keyed by one field: which want which value.
#[derive(Default)]
struct Index {
    num: HashMap<u64, Vec<usize>>,
    text: HashMap<Vec<u8>, Vec<usize>>,
    no_case: HashMap<Vec<u8>, Vec<usize>>,
}

impl Index {
    /// Calls `f` with each signal that wants `v`.
    fn each(&self, v: &FieldValue<'_>, f: &mut impl FnMut(usize)) {
        match v {
            FieldValue::U64(n) => self.num.get(n).into_iter().flatten().copied().for_each(&mut *f),
            FieldValue::Str(s) => {
                self.text.get(s.as_ref()).into_iter().flatten().copied().for_each(&mut *f);
                if !self.no_case.is_empty() {
                    let mut buf = [0u8; 32];
                    let lower: std::borrow::Cow<'_, [u8]> = match buf.get_mut(..s.len()) {
                        Some(b) => {
                            b.copy_from_slice(s);
                            b.make_ascii_lowercase();
                            std::borrow::Cow::Borrowed(b)
                        }
                        None => std::borrow::Cow::Owned(s.to_ascii_lowercase()),
                    };
                    self.no_case.get(lower.as_ref()).into_iter().flatten().copied().for_each(&mut *f);
                }
            }
            FieldValue::F64(_) => {}
        }
    }
}

/// One signal's events in a stretch of rows, as [`SignalDef::events`]
/// keeps them: numbers in two arrays, other values with their rows.
enum Found {
    Num(Vec<LineNo>, Vec<f64>),
    Values(Vec<(LineNo, SignalValue)>),
}

impl Found {
    fn new(kind: SignalKind) -> Found {
        match kind {
            SignalKind::Enum => Found::Values(Vec::new()),
            _ => Found::Num(Vec::new(), Vec::new()),
        }
    }

    fn push(&mut self, row: LineNo, v: SignalValue) {
        match self {
            Found::Num(rows, nums) => {
                rows.push(row);
                nums.push(if let SignalValue::Num(n) = v { n } else { f64::NAN });
            }
            Found::Values(vs) => vs.push((row, v)),
        }
    }

    /// The pieces of consecutive stretches as one signal's events.
    fn join(pieces: Vec<Found>, kind: SignalKind) -> Events {
        let mut all = Found::new(kind);
        for p in pieces {
            match (&mut all, p) {
                (Found::Num(rows, nums), Found::Num(r, n)) => {
                    rows.extend(r);
                    nums.extend(n);
                }
                (Found::Values(vs), Found::Values(v)) => vs.extend(v),
                _ => unreachable!("one kind per signal"),
            }
        }
        match all {
            Found::Num(rows, nums) => Events::numbers(rows, nums, kind),
            Found::Values(vs) => Events::build(vs, kind),
        }
    }
}

impl Signals {
    /// Collect the events of every signal on `source` that hasn't been
    /// collected yet: keyed signals together, a pass per file (module
    /// docs), then the others one by one. A definition that doesn't fit
    /// the source is skipped here: its own [`SignalDef::series`] says why.
    pub fn collect(&self, source: &dyn LogSource, progress: &Progress) -> crate::Result<()> {
        match source.parts() {
            Some(ps) => {
                for p in &ps {
                    self.collect_keyed(p.source.as_ref(), progress)?;
                }
            }
            None => self.collect_keyed(source, progress)?,
        }
        for d in self.iter() {
            if let Err(crate::Error::Cancelled) = d.series(source, progress) {
                return Err(crate::Error::Cancelled);
            }
        }
        Ok(())
    }

    /// The keyed signals' events in one file (a source without parts).
    fn collect_keyed(&self, source: &dyn LogSource, progress: &Progress) -> crate::Result<()> {
        let key = source_key(source);
        // (bound definition, the field it is keyed by, the value it wants)
        let mut keyed: Vec<(SignalDef, FieldId, Want)> = Vec::new();
        for d in self.iter() {
            if !matches!(d.builtin, Builtin::No) || d.cached(key).is_some() {
                continue;
            }
            // Copies share their events: collected once.
            if keyed.iter().any(|(k, _, _)| k.shares_events_with(d)) {
                continue;
            }
            let Ok(bound) = d.bind(source.schema()) else { continue };
            if let Some((id, want)) = required(bound.query.expr()) {
                keyed.push((bound, id, want));
            }
        }
        // One signal alone gains nothing (its own pass may take a faster path).
        if keyed.len() < 2 {
            return Ok(());
        }
        let mut fields: Vec<(FieldId, Index)> = Vec::new();
        for (i, (_, id, want)) in keyed.iter().enumerate() {
            let at = match fields.iter().position(|(f, _)| f == id) {
                Some(at) => at,
                None => {
                    fields.push((*id, Index::default()));
                    fields.len() - 1
                }
            };
            let index = &mut fields[at].1;
            match want {
                Want::Num(n) => index.num.entry(*n).or_default().push(i),
                Want::Str(s) => index.text.entry(s.clone()).or_default().push(i),
                Want::NoCase(s) => index.no_case.entry(s.clone()).or_default().push(i),
            }
        }

        let n = source.line_count();
        let tasks: Vec<Range<LineNo>> =
            (0..n.div_ceil(LINES_PER_TASK)).map(|t| t * LINES_PER_TASK..((t + 1) * LINES_PER_TASK).min(n)).collect();
        let stretches: Vec<Vec<Found>> = tasks
            .into_par_iter()
            .map(|rows| {
                if progress.is_cancelled() {
                    return Err(crate::Error::Cancelled);
                }
                let len = rows.end - rows.start;
                let mut found: Vec<Found> = keyed.iter().map(|(d, _, _)| Found::new(d.kind)).collect();
                for row in rows {
                    let mut ev: Option<Event<'_>> = None;
                    for (id, index) in &fields {
                        let Some(v) = source.field(row, *id) else { continue };
                        index.each(&v, &mut |i| {
                            let ev = ev.get_or_insert_with(|| Event::from_source(source, row));
                            if let Some(value) = keyed[i].0.value(ev) {
                                found[i].push(row, value);
                            }
                        });
                    }
                }
                progress.add(len);
                Ok(found)
            })
            .collect::<crate::Result<_>>()?;

        // Per signal: its stretches in order, then into its cache.
        let mut per_signal: Vec<Vec<Found>> = keyed.iter().map(|_| Vec::with_capacity(stretches.len())).collect();
        for stretch in stretches {
            for (i, f) in stretch.into_iter().enumerate() {
                per_signal[i].push(f);
            }
        }
        per_signal.into_par_iter().zip(keyed.par_iter()).for_each(|(pieces, (d, _, _))| {
            d.store(key, Found::join(pieces, d.kind));
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::dlt::writer::{Msg, Payload, Value};
    use crate::{Fibex, Level, OpenOptions};

    /// Four described messages of ECU1 (one id described for any ECU),
    /// arguments of every kind; conventional signal names.
    const FIBEX: &str = r#"<fx:FIBEX xmlns:fx="f" xmlns:ho="h"><fx:ELEMENTS>
      <fx:PDUS>
        <fx:PDU ID="t1"><ho:DESC>a:</ho:DESC></fx:PDU>
        <fx:PDU ID="u16"><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="i1"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_UINT16"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>
        <fx:PDU ID="i32"><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="i2"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_SINT32"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>
        <fx:PDU ID="f32"><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="i3"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_FLOA32"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>
        <fx:PDU ID="str"><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="i4"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_STRG_ASCII"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>
        <fx:PDU ID="u8"><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="i5"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_UINT8"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>
      </fx:PDUS>
      <fx:FRAMES>
        <fx:FRAME ID="ID_1"><fx:PDU-INSTANCES>
          <fx:PDU-INSTANCE ID="a"><fx:PDU-REF ID-REF="t1"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
          <fx:PDU-INSTANCE ID="b"><fx:PDU-REF ID-REF="u16"/><fx:SEQUENCE-NUMBER>1</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
          <fx:PDU-INSTANCE ID="c"><fx:PDU-REF ID-REF="i32"/><fx:SEQUENCE-NUMBER>2</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
        </fx:PDU-INSTANCES><fx:MANUFACTURER-EXTENSION><APPLICATION_ID>ENG</APPLICATION_ID><CONTEXT_ID>CTL</CONTEXT_ID></fx:MANUFACTURER-EXTENSION></fx:FRAME>
        <fx:FRAME ID="ID_2"><fx:PDU-INSTANCES>
          <fx:PDU-INSTANCE ID="d"><fx:PDU-REF ID-REF="f32"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
          <fx:PDU-INSTANCE ID="e"><fx:PDU-REF ID-REF="str"/><fx:SEQUENCE-NUMBER>1</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
        </fx:PDU-INSTANCES><fx:MANUFACTURER-EXTENSION><APPLICATION_ID>NAV</APPLICATION_ID></fx:MANUFACTURER-EXTENSION></fx:FRAME>
        <fx:FRAME ID="ID_3"><fx:PDU-INSTANCES>
          <fx:PDU-INSTANCE ID="f"><fx:PDU-REF ID-REF="u8"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
        </fx:PDU-INSTANCES></fx:FRAME>
      </fx:FRAMES>
    </fx:ELEMENTS></fx:FIBEX>"#;

    /// `n` messages: the described ones of two ECUs (some big endian), and
    /// verbose text of NAV and SYS with a number.
    fn trace(n: u32, seed: u64) -> Vec<u8> {
        let mut rng = seed;
        let mut out = Vec::new();
        for i in 0..n {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let ecu = if rng.is_multiple_of(5) { "ECU2" } else { "ECU1" };
            let be = rng.is_multiple_of(7);
            let x = (rng >> 20) as u32;
            let nv = |id: u32, data: Vec<u8>| Msg {
                ext: None,
                big_endian: be,
                payload: Payload::NonVerbose { id, data },
                ..Msg::log(Level::Info, ecu, "", "", vec![])
            };
            let bytes = |v: &[u8]| if be { v.iter().rev().copied().collect() } else { v.to_vec() };
            let mut m = match rng % 6 {
                0 => {
                    let mut d = bytes(&(x as u16).to_le_bytes());
                    d.extend(bytes(&(x as i32).wrapping_sub(1000).to_le_bytes()));
                    nv(1, d)
                }
                1 => {
                    let mut d = bytes(&(x as f32 / 8.0).to_le_bytes());
                    let s = ["ON", "OFF", "IDLE"][(x % 3) as usize];
                    d.extend(bytes(&(s.len() as u16 + 1).to_le_bytes()));
                    d.extend(s.as_bytes());
                    d.push(0);
                    nv(2, d)
                }
                2 => nv(3, vec![x as u8]),
                3 => nv(9, vec![1, 2]),
                _ => {
                    let apid = if rng.is_multiple_of(2) { "NAV" } else { "SYS" };
                    Msg::log(Level::Warn, ecu, apid, "CTX", vec![Value::Str(format!("n={}", x % 500)).into()])
                }
            };
            m.mcnt = i as u8;
            m.secs = 1_760_745_600 + i / 10;
            m.encode(&mut out);
        }
        out
    }

    fn definitions() -> Signals {
        let mut s = Signals::default();
        let arg = |name: &str, kind, q: &str, i| SignalDef::argument(name, kind, q, i, "test").unwrap();
        let ecu1 = |mid: u32| format!(r#"mid == {mid} AND ecu == "ECU1""#);
        s.set(arg("a_u16", SignalKind::Num, &ecu1(1), 0));
        s.set(arg("a_i32", SignalKind::Num, &ecu1(1), 1));
        s.set(arg("b_f32", SignalKind::Num, &ecu1(2), 0));
        s.set(arg("b_str", SignalKind::Enum, &ecu1(2), 1));
        s.set(arg("c_u8_enum", SignalKind::Enum, &ecu1(3), 0));
        s.set(arg("a_any_ecu", SignalKind::Num, "mid == 1", 0));
        s.set(arg("c_ecu2", SignalKind::Num, r#"mid == 3 AND ecu == "ECU2""#, 0));
        // Regex signals keyed by a text field, without case, by a number.
        for spec in [
            r"nav=apid == NAV AND /n=(?<nav>\d+)/",
            r"ecu2:enum=ecu == ECU2 AND /n=(?<ecu2>\d+)/",
            r"seven=mcnt == 7 AND /n=(?<seven>\d+)/",
            // Not keyed: a pass of their own.
            r"any=/n=(?<any>\d+)/",
            r"not_nav=apid != NAV AND /n=(?<not_nav>\d+)/",
            r"either=(apid == NAV OR apid == SYS) AND /n=(?<either>\d+)/",
        ] {
            s.set(SignalDef::parse_spec(spec).unwrap());
        }
        s
    }

    fn events(s: &Signals, name: &str, source: &dyn LogSource) -> Vec<(LineNo, SignalValue)> {
        let d = s.get(name).unwrap();
        d.cached_series(source)
            .unwrap_or_else(|| panic!("{name} collected"))
            .iter()
            .map(|(r, v)| (r, v.to_value()))
            .collect()
    }

    /// Every signal's events collected together equal the ones each
    /// collects on its own, on one file, on two as one view and on two
    /// merged by time.
    #[test]
    fn signals_collected_together_have_the_same_events() {
        let dir = std::env::temp_dir().join(format!("nerka-collect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fibex = dir.join("ecu.xml");
        std::fs::write(&fibex, FIBEX).unwrap();
        let (a, b) = (dir.join("a.dlt"), dir.join("b.dlt"));
        // More rows than a task, so stretches join.
        std::fs::write(&a, trace(150_000, 0x9E37_79B9_7F4A_7C15)).unwrap();
        std::fs::write(&b, trace(20_000, 7)).unwrap();
        let options = OpenOptions { fibex: Some(Arc::new(Fibex::load(&[fibex]).unwrap())), ..Default::default() };
        let p = Progress::new();
        let views: Vec<(&str, Box<dyn LogSource>)> = vec![
            ("one file", crate::open_with(&a, &options, &p).unwrap()),
            ("two files", crate::open_many(&[a.clone(), b.clone()], &options, &p).unwrap()),
            ("merged by time", crate::open_merged_by_time(&[a.clone(), b.clone()], &options, &p).unwrap().0),
        ];
        for (what, source) in &views {
            let alone = definitions();
            for d in alone.iter() {
                d.series(&**source, &p).unwrap();
            }
            let together = definitions();
            together.collect(&**source, &p).unwrap();
            for d in alone.iter() {
                let (x, y) = (events(&alone, d.name(), &**source), events(&together, d.name(), &**source));
                assert!(!x.is_empty(), "{what}: {} has events", d.name());
                assert!(x == y, "{what}: {} differs ({} events alone, {} together)", d.name(), x.len(), y.len());
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keys_are_required_equalities() {
        let file = crate::DltFile::from_bytes("t.dlt", trace(10, 1));
        let schema = file.schema();
        let key = |q: &str| required(Query::compile_for(q, schema).unwrap().expr()).map(|(_, w)| w);
        assert_eq!(key(r#"mid == 11 AND ecu == "E1""#), Some(Want::Num(11)), "a number first");
        assert_eq!(key("apid == NAV"), Some(Want::Str(b"NAV".to_vec())));
        assert_eq!(key("mid != 11"), None);
        assert_eq!(key("mid == 1 OR mid == 2"), None, "not required");
        assert_eq!(key("NOT mid == 1"), None);
        assert_eq!(key("mid > 1"), None);
        // A name (`proto`) compares without case.
        let names = [crate::FieldDef::new("proto", crate::FieldKind::Name, false)];
        let (id, want) = required(Query::compile_for("proto == DLT", &names).unwrap().expr()).unwrap();
        assert_eq!(want, Want::NoCase(b"dlt".to_vec()));
        let mut index = Index::default();
        index.no_case.insert(b"dlt".to_vec(), vec![4]);
        let found = |v: &[u8]| {
            let mut out = Vec::new();
            index.each(&FieldValue::Str(v.to_vec().into()), &mut |i| out.push(i));
            out
        };
        assert_eq!((id, found(b"DLT"), found(b"dlt"), found(b"SOMEIP")), (0, vec![4], vec![4], vec![]));
        assert_eq!(found(&[b'D'; 40]), Vec::<usize>::new(), "longer than the buffer");
    }

    #[test]
    fn a_cancelled_collection_says_so() {
        let file = crate::DltFile::from_bytes("t.dlt", trace(1000, 3));
        let p = Progress::new();
        p.cancel();
        let s = definitions();
        assert!(matches!(s.collect(&file, &p), Err(crate::Error::Cancelled)));
        assert!(s.get("a_u16").unwrap().cached_series(&file).is_none(), "nothing kept");
    }
}
