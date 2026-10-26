#!/usr/bin/env python3
"""Generate the sdkconfig crate source and the --cfg argfile for Rust crates.

usage: sdkconfig_rs.py <sdkconfig.json> <kconfig_menus.json> <out.rs> <out.args>
"""

import json
import sys


def kconfig_types(menus):
    types = {}
    stack = list(menus)
    while stack:
        node = stack.pop()
        if node.get("name"):
            types[node["name"]] = node["type"]
        stack.extend(node.get("children") or [])
    return types


def int_type(value, signed):
    if signed:
        return "i32" if -(1 << 31) <= value < (1 << 31) else "i64"
    return "u32" if 0 <= value < (1 << 32) else "u64"


def write_if_changed(path, content):
    try:
        with open(path) as f:
            if f.read() == content:
                return
    except FileNotFoundError:
        pass
    with open(path, "w") as f:
        f.write(content)


def main():
    json_path, menus_path, rs_path, args_path = sys.argv[1:]
    with open(json_path) as f:
        config = json.load(f)
    with open(menus_path) as f:
        types = kconfig_types(json.load(f))

    consts = []
    bools = []
    enabled = []
    for name in sorted(config):
        value = config[name]
        ident = f"CONFIG_{name}"
        if isinstance(value, bool):
            bools.append(ident)
            if value:
                enabled.append(ident)
            consts.append(f"pub const {ident}: bool = {'true' if value else 'false'};")
        elif isinstance(value, int):
            ty = int_type(value, types.get(name) != "hex")
            consts.append(f"pub const {ident}: {ty} = {value};")
        elif isinstance(value, str):
            literal = value.replace("\\", "\\\\").replace('"', '\\"')
            consts.append(f'pub const {ident}: &str = "{literal}";')

    rs = "#![no_std]\n" + "\n".join(consts) + "\n"
    args = [f"--check-cfg=cfg({','.join(bools)})"]
    args += [f"--cfg={ident}" for ident in enabled]
    write_if_changed(rs_path, rs)
    write_if_changed(args_path, "\n".join(args) + "\n")


if __name__ == "__main__":
    main()
