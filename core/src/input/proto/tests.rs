//! The protocol layer on its own; captures with several protocols are
//! tested in `capture/tests.rs`.

use super::someip::{SomeIp, encode};
use super::*;

const AT: Origin<'static> = Origin { secs: 0, usecs: 0, channel: NO_CHANNEL };

fn split(p: &dyn Protocol, datagram: &[u8]) -> (Vec<usize>, usize) {
    let mut found = Vec::new();
    let skipped = p.split(datagram, &mut found);
    (found, skipped)
}

#[test]
fn someip_messages_back_to_back() {
    let a = encode(0x1234, 0x8001, 1, 2, 0x02, 0, &[0xaa; 5]);
    let b = encode(0x1234, 0x0001, 1, 3, 0x00, 0, &[]);
    let datagram = [a.clone(), b.clone(), vec![0xff; 3]].concat();
    assert_eq!(split(&SomeIp, &datagram), (vec![0, a.len()], 3));
    // A length past the datagram's end, or another protocol version: no message.
    let mut cut = a.clone();
    cut.truncate(a.len() - 1);
    assert_eq!(split(&SomeIp, &cut), (vec![], cut.len()));
    let mut v2 = a.clone();
    v2[12] = 2;
    assert_eq!(split(&SomeIp, &v2), (vec![], v2.len()));

    let mut out = Vec::new();
    SomeIp.render(&datagram[a.len()..], &AT, &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), "0x1234\t0x0001\t0x0001\t0x0003\tREQUEST\tE_OK\t[0]");
    let num = |id| SomeIp.field(&a, &AT, id);
    assert_eq!((num(0), num(7)), (Some(FieldValue::U64(0x1234)), Some(FieldValue::U64(5))));
    assert_eq!(SomeIp.level(&encode(1, 2, 3, 4, 0x80, 0x06, &[]), &AT), Some(Level::Error), "E_TIMEOUT");
}

#[test]
fn framed_entries_keep_every_part() {
    let f = Framed::new(12345, true, 2, 77, 9, -1).unwrap();
    assert_eq!((f.offset(), f.in_side(), f.proto(), f.channel(), f.secs, f.usecs), (12345, true, 2, 77, 9, -1));
    assert_eq!(size_of::<Framed>(), 16);
}

/// Several protocols: shared columns, `proto` first, every protocol's
/// fields once (`type` and `payload` are DLT's and SOME/IP's alike).
#[test]
fn a_view_of_several_protocols_unites_their_fields() {
    let protos: Vec<Arc<dyn Protocol>> =
        vec![Arc::new(crate::input::dlt::Dlt::default()), Arc::new(SomeIp), Arc::new(syslog::Syslog)];
    let v = View::new(&protos, &[0, 1]);
    assert!(v.mixed);
    let names: Vec<&str> = v.schema.iter().map(|d| d.name).collect();
    assert_eq!(&names[..3], ["proto", "ecu", "apid"]);
    for (i, name) in names.iter().enumerate() {
        assert_eq!(names.iter().position(|n| n == name), Some(i), "{name} once");
    }
    let ty = names.iter().position(|n| *n == "type").unwrap();
    assert!(v.fields[0][ty].is_some() && v.fields[1][ty].is_some() && v.fields[2][ty].is_none());
    let service = names.iter().position(|n| *n == "service").unwrap();
    assert!(v.fields[0][service].is_none() && v.fields[1][service].is_some());
    // One protocol: its own columns, `proto` last (DLT's field ids stay).
    let one = View::new(&protos, &[0]);
    assert!(!one.mixed);
    assert_eq!(one.proto_field, one.schema.len() - 1);
    assert_eq!(one.columns, crate::input::dlt::COLUMNS);
}
