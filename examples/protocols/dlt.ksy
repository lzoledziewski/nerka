# DLT (AUTOSAR PRS_Dlt) messages as a protocol definition: the standard and
# extended headers and the payload (verbose arguments, control messages,
# non-verbose ids; no FIBEX). An example of a full definition, and a test:
# core/tests/ksy_dlt.rs compares it with nerka's native DLT reader, and
# tools/bench.sh (KSY=1) benchmarks the two (docs/dev/protocol-definitions.md; the language: docs/user/protocol-definitions.md).
# Messages start at their standard header (DLT over UDP has no storage
# header). Its name is DLTK, so it can run next to native DLT:
#
#   nerka --protocol examples/protocols/dlt.ksy --protocol-port DLTK=3491 filter 'proto == DLTK AND apid == NAV' trace.pcap
meta:
  id: dlt_message
  title: AUTOSAR DLT message, from its standard header on
  license: MIT OR Apache-2.0
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
      # 1 control, 2 verbose, 3 non-verbose
      kind: { value: "_root.has_ext and ext.mstp == 3 ? 1 : verbose ? 2 : 3" }
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
      endian:                    # the payload's byte order: MSBF
        switch-on: _root.msbf
        cases:
          true: be
          false: le
    seq:
      - id: content
        type:
          switch-on: _parent.kind
          cases:
            1: control
            2: verbose
            3: non_verbose
        size-eos: true
        -nerka-partial: "{_rest:hex}"     # too short for its kind: hex
    instances:
      nv_id:                     # message id of a non-verbose message
        pos: 0
        type: u4
        if: not _parent.verbose and not (_root.has_ext and _parent.ext.mstp == 3) and _io.size >= 4
    -nerka-text: "{content}"

  # Verbose: NOAR self-describing arguments. One that can't be read (an
  # array, a struct, fixed point, a cut value) ends the list; the rest is
  # shown as hex.
  verbose:
    seq:
      - id: args
        type: arg
        repeat: expr
        repeat-expr: _root.body.ext.noar
        -nerka-partial: "<undecoded> {_rest:hex}"
    -nerka-text: "{args}"
  arg:
    seq:
      - { id: ti, type: u4 }     # type info
      - id: value
        type:
          switch-on: kind
          cases:
            1: text_arg
            2: bool_arg
            3: float_arg
            4: sint_arg
            5: uint_arg
            6: raw_arg
    instances:
      vari: { value: (ti & 0x800) != 0 }
      tyle: { value: ti & 0x0f }
      named: { value: vari and (ti & 0x2000) == 0 }   # trace info has no name
      # 0: not read (array, struct, fixed point, no type)
      kind:
        value: >-
          (ti & 0x4100) != 0 ? 0 :
          (ti & 0x2600) != 0 ? ((ti & 0x400) != 0 ? 6 : 1) :
          (ti & 0x10) != 0 ? 2 :
          ((ti & 0xe0) == 0 or (ti & 0x1000) != 0) ? 0 :
          (ti & 0x80) != 0 ? 3 :
          (ti & 0x20) != 0 ? 4 : 5
    -nerka-text: "{value}"
  name:
    seq:
      - { id: len, type: u2 }
      - { id: text, size: len }
  names:                         # name and unit of a number
    seq:
      - { id: name_len, type: u2 }
      - { id: unit_len, type: u2 }
      - { id: name, size: name_len }
      - { id: unit, size: unit_len }
  text_arg:
    seq:
      - { id: len, type: u2 }
      - { id: name, type: name, if: _parent.named }
      - { id: data, type: str, size: len, encoding: UTF-8 }
    -nerka-text: "[{name.text:str}=]{data:str}"
  raw_arg:
    seq:
      - { id: len, type: u2 }
      - { id: name, type: name, if: _parent.named }
      - { id: data, size: len }
    -nerka-text: "[{name.text:str}=]{data:hex}"
  bool_arg:
    seq:
      - { id: name, type: name, if: _parent.vari }
      - { id: v, type: u1 }
    -nerka-text: "[{name.text:str}=]{v != 0}"
  float_arg:
    seq:
      - { id: names, type: names, if: _parent.vari }
      - id: v
        type:
          switch-on: _parent.tyle
          cases: { 3: f4, 4: f8 }
    -nerka-text: "[{names.name:str}=]{v}[ {names.unit:str}]"
  sint_arg:
    seq:
      - { id: names, type: names, if: _parent.vari }
      - id: v
        type:
          switch-on: _parent.tyle
          cases: { 1: s1, 2: s2, 3: s4, 4: s8, 5: int128 }
    -nerka-text: "[{names.name:str}=]{v}[ {names.unit:str}]"
  uint_arg:
    seq:
      - { id: names, type: names, if: _parent.vari }
      - id: v
        type:
          switch-on: _parent.tyle
          cases: { 1: u1, 2: u2, 3: u4, 4: u8, 5: uint128 }
    -nerka-text: "[{names.name:str}=]{v}[ {names.unit:str}]"
  int128:
    seq:
      - { id: b, size: 16 }
    -nerka-text: "{b:sdec}"
  uint128:
    seq:
      - { id: b, size: 16 }
    -nerka-text: "{b:dec}"

  # Control: the service, a response's status, a version string.
  control:
    seq:
      - { id: sid, type: u4, enum: service }
      - { id: status, type: u1, if: response and _io.size >= 5 }
      - { id: version, type: version, if: version_ok }
      - { id: rest, size-eos: true, if: not version_ok }
    instances:
      response: { value: _root.body.ext.mtin == 2 }
      vlen:
        pos: 5
        type: u4
        if: response and sid == service::get_software_version and _io.size >= 9
      version_ok:
        value: response and sid == service::get_software_version and _io.size >= 9 and vlen <= _io.size - 9
      status_text:
        value: "status == 0 ? 'ok' : status == 1 ? 'not_supported' : status == 2 ? 'error' : 'status?'"
        if: response and _io.size >= 5
    -nerka-text: "[[[{sid:name}|service {sid:hex}][ {status_text}]]][{version}][ {rest:hex}]"
  version:
    seq:
      - { id: len, type: u4 }
      - { id: text, type: str, size: len, encoding: UTF-8 }
    -nerka-text: " {text:str}"

  # Non-verbose without a description: the message id, then hex.
  non_verbose:
    seq:
      - { id: id, type: u4 }
      - { id: rest, size-eos: true }
    -nerka-text: "[[{id}]][ {rest:hex}]"

enums:
  service:
    0x01: set_log_level
    0x02: set_trace_status
    0x03: get_log_info
    0x04: get_default_log_level
    0x05: store_config
    0x06: reset_to_factory_default
    0x0a: set_message_filtering
    0x11: set_default_log_level
    0x12: set_default_trace_status
    0x13: get_software_version
    0x15: get_default_trace_status
    0x17: get_log_channel_names
    0x1f: get_trace_status
    0xf01: unregister_context
    0xf02: connection_info
    0xf03: timezone
    0xf04: marker

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
    payload: { from: body.payload, costly: true }   # decoded as native DLT does (no FIBEX)
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
