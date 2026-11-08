# A made-up sensor datagram, as a small example of a protocol definition
# (doc/protocol-definitions.md). One reading per UDP datagram:
#
#   id u2, seq u4, kind u1 (1 temperature, 2 pressure), value s2 (tenths),
#   then a free text note up to the datagram's end.
#
#   nerka --protocol examples/protocols/sensor.ksy filter 'kind == pressure AND value > 90' capture.pcap
meta:
  id: sensor_reading
  license: MIT OR Apache-2.0
  endian: le
seq:
  - { id: sensor, type: u2 }
  - { id: seq, type: u4 }
  - { id: kind, type: u1, enum: kind }
  - { id: value, type: s2 }
  - { id: note, type: str, size-eos: true, encoding: UTF-8 }
enums:
  kind:
    1: temperature
    2: pressure
-nerka:
  name: SENSOR
  framing: datagram
  match: [ 41000 ]                    # its default port; --route SENSOR:RULE replaces it
  fields:
    sensor: sensor
    seq: seq
    kind: { from: kind, kind: name }
    value: value / 10.0
    note: note
  level:
    from: "kind == kind::temperature and value > 900 ? 2 : 4"   # over 90.0 degrees: an error
    map: { 2: error, 4: info }
  columns:
    - { name: sensor, width: 6 }
    - { name: kind, width: 11 }
    - { name: value, from: value / 10.0, decimals: 1, width: 6 }
    - note
