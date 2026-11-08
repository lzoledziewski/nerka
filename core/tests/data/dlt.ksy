# DLT (AUTOSAR PRS_Dlt) messages as a protocol definition: the standard and
# extended headers, the payload as hex. A test fixture: it is compared with
# Nerka's native DLT reader (core/tests/ksy_dlt.rs) and benchmarked against
# it (doc/protocol-definitions.md). Messages start at their standard header
# (DLT over UDP has no storage header).
meta:
  id: dlt_message
  title: AUTOSAR DLT message, from its standard header on
  endian: be                     # LEN, session id and timestamp are always big-endian
  bit-endian: be
seq:
  - id: htyp
    type: u1
    valid:
      expr: (_ & 0xe0) == 0x20   # version 1
  - id: mcnt
    type: u1
  - id: len                      # the whole message, standard header included
    type: u2
    valid:
      min: 4
  - id: body
    size: len - 4
    type: body
instances:
  has_ext:  { value: (htyp & 0x01) != 0 }   # UEH
  msbf:     { value: (htyp & 0x02) != 0 }   # payload big-endian
  has_ecu:  { value: (htyp & 0x04) != 0 }   # WEID
  has_seid: { value: (htyp & 0x08) != 0 }   # WSID
  has_tmsp: { value: (htyp & 0x10) != 0 }   # WTMS
types:
  body:
    seq:
      - { id: ecu, type: str, size: 4, terminator: 0, encoding: ASCII, if: _root.has_ecu }
      - { id: seid, type: u4, if: _root.has_seid }
      - { id: tmsp, type: u4, if: _root.has_tmsp }   # uptime in 0.1 ms
      - { id: ext, type: ext_header, if: _root.has_ext }
      - { id: payload, type: payload, size-eos: true }
    instances:
      verbose: { value: _root.has_ext and ext.verb }
  ext_header:
    seq:
      - { id: mtin, type: b4 }   # MSIN, from its most significant bit
      - { id: mstp, type: b3 }
      - { id: verb, type: b1 }
      - { id: noar, type: u1 }
      - { id: apid, type: str, size: 4, terminator: 0, encoding: ASCII }
      - { id: ctid, type: str, size: 4, terminator: 0, encoding: ASCII }
  payload:
    meta:
      endian:
        switch-on: _root.msbf
        cases:
          true: be
          false: le
    instances:
      nv_id:                     # message id of a non-verbose message
        pos: 0
        type: u4
        if: not _parent.verbose and not (_root.has_ext and _parent.ext.mstp == 3) and _io.size >= 4

-nerka:
  name: DLTK
  framing: { length: len }
  fields:
    ecu: { from: body.ecu, else: origin.channel }   # no WEID: the container's ECU
    apid: body.ext.apid
    ctid: body.ext.ctid
    type:
      from: body.ext.mstp
      map: { 0: log, 1: app_trace, 2: nw_trace, 3: control }
    mcnt: mcnt
    tmsp: body.tmsp
    seid: body.seid
    mid: body.payload.nv_id
    mode: "body.verbose ? 'verbose' : 'non-verbose'"
    payload: { from: body.payload, render: hex }
  level:
    if: has_ext and body.ext.mstp == 0
    from: body.ext.mtin
    map: { 1: fatal, 2: error, 3: warn, 4: info, 5: debug, 6: trace }
  uptime: body.tmsp / 10000.0
  columns:
    - { name: tmsp, from: body.tmsp / 10000.0, decimals: 4, width: 11 }
    - { name: ecu, width: 4 }
    - { name: apid, width: 4 }
    - { name: ctid, width: 4 }
    - name: type                 # the level of a log message, else the type
      width: 9
      if: has_ext and body.ext.mstp == 0
      from: body.ext.mtin
      map: { 1: fatal, 2: error, 3: warn, 4: info, 5: debug, 6: verbose }
      else:
        from: body.ext.mstp
        map: { 0: log, 1: app_trace, 2: nw_trace, 3: control }
    - payload
