# Matter node dumps

Each file is one node as the Matter server describes it (`MatterNodeData`): its id, whether it's
reachable, and every attribute it read, keyed `endpoint/cluster/attribute`. `src/map/` and
`src/lib.rs` test against them.

- `lab_*.json`: the dev lab's emulated devices (`dev/lab/matter/device.mjs`), commissioned onto a
  matter-server 1.4.0 and dumped with `get_nodes`.
- `tapo_p110m.json`: a Tapo P110M plug, dumped the same way and cut down to its Basic
  Information and its plug's endpoint. Its serial number and unique id are made up.
- Every other file is from Home Assistant's Matter integration tests
  (`tests/components/matter/fixtures/nodes/`, <https://github.com/home-assistant/core>),
  copyright the Home Assistant authors, under the Apache License 2.0. They are unchanged.
