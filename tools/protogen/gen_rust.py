"""Rust generator: <name>.pb.rs, plus the pb.rs runtime it includes."""

from pathlib import Path

from c_names import to_camel
from proto import ProtoError

SCALARS = {
    "int32": ("i32", "int32", "|v: i32| v as i64 as u64", "|v: u64| v as i32"),
    "int64": ("i64", "int64", "|v: i64| v as u64", "|v: u64| v as i64"),
    "uint32": ("u32", "uint32", "|v: u32| u64::from(v)", "|v: u64| v as u32"),
    "uint64": ("u64", "uint64", "|v: u64| v", "|v: u64| v"),
    "bool": ("bool", "bool", "|v: bool| u64::from(v)", "|v: u64| v != 0"),
}

KEYWORDS = {
    "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "static", "struct", "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become",
    "box", "do", "final", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
}
UNRAWABLE = {"self", "Self", "super", "crate", "_"}

RUNTIME = Path(__file__).with_name("pb.rs")


def ident(name, where):
    if name in UNRAWABLE:
        raise ProtoError(f"{where}: {name!r} cannot be a Rust identifier")
    return f"r#{name}" if name in KEYWORDS else name


def check(proto):
    for m in proto.messages:
        for f in m.fields:
            where = f"{proto.name}: {m.name}.{f.name}"
            if f.is_scalar and f.type_name not in SCALARS and f.type_name != "bytes":
                raise ProtoError(f"{where}: type {f.type_name} is not supported")
        numbers = sorted(f.number for f in m.fields)
        for o in m.oneofs:
            members = sorted(f.number for f in o.fields)
            span = [n for n in numbers if members[0] <= n <= members[-1]]
            if span != members:
                raise ProtoError(f"{proto.name}: {m.name}.{o.name}: members interleaved with other fields "
                                 "are not supported")
    names = {}
    for t in proto.enums + proto.messages:
        names.setdefault(to_camel(t.name), []).append(t.name)
    for m in proto.messages:
        for o in m.oneofs:
            names.setdefault(oneof_type(m, o), []).append(f"{m.name}.{o.name}")
    for name, owners in names.items():
        if len(owners) > 1:
            raise ProtoError(f"{proto.name}: {', '.join(owners)} all map to Rust type {name}")
    for name in names:
        if name in ("pb", "Vec", "Box", "Option"):
            raise ProtoError(f"{proto.name}: Rust type {name} is reserved")


def oneof_type(m, o):
    return to_camel(m.name) + to_camel(o.name)


def borrowing(proto):
    """Messages that need a lifetime: bytes, a borrowing message, or a lazy oneof member."""
    out = set()
    changed = True
    while changed:
        changed = False
        for m in proto.messages:
            if m.name in out:
                continue
            for f in m.fields:
                if (f.type_name == "bytes" or (f.oneof and f.is_message) or
                        (f.is_message and f.type.name in out)):
                    out.add(m.name)
                    changed = True
                    break
    return out


class Gen:
    def __init__(self, proto):
        self.proto = proto
        self.borrowing = borrowing(proto)
        self.out = []

    def w(self, text=""):
        self.out.append(text + "\n")

    def type_name(self, t):
        name = to_camel(t.name)
        return f"{name}<'a>" if t.name in self.borrowing else name

    def elem_type(self, f):
        if f.is_message:
            return self.type_name(f.type)
        if f.is_enum:
            return to_camel(f.type.name)
        if f.type_name == "bytes":
            return "&'a [u8]"
        return SCALARS[f.type_name][0]

    def member_type(self, f):
        if f.repeated:
            return f"Vec<{self.elem_type(f)}>"
        if f.is_message:
            if f.oneof:
                return f"pb::Lazy<'a, {self.elem_type(f)}>"
            return f"Option<Box<{self.elem_type(f)}>>"
        return self.elem_type(f)

    def to_wire(self, f):
        if f.is_enum:
            return f"|v: {to_camel(f.type.name)}| v.0 as i64 as u64"
        return SCALARS[f.type_name][2]

    def from_wire(self, f):
        if f.is_enum:
            return f"|v: u64| {to_camel(f.type.name)}(v as i32)"
        return SCALARS[f.type_name][3]

    def scalar_expr(self, f, v):
        """u64 wire value of a scalar or enum expression."""
        if f.is_enum:
            return f"{v}.0 as i64 as u64"
        return {
            "int32": f"{v} as i64 as u64", "int64": f"{v} as u64", "uint32": f"u64::from({v})",
            "uint64": v, "bool": f"u64::from({v})",
        }[f.type_name]

    def is_default(self, f, v):
        if f.is_message:
            return None
        if f.is_enum:
            return f"{v}.0 != 0"
        if f.type_name == "bytes":
            return f"!{v}.is_empty()"
        if f.type_name == "bool":
            return v
        return f"{v} != 0"

    def enum(self, e):
        name = to_camel(e.name)
        self.w("#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]")
        self.w(f"pub struct {name}(pub i32);")
        self.w()
        self.w("#[allow(non_upper_case_globals)]")
        self.w(f"impl {name} {{")
        for v in e.values:
            self.w(f"    pub const {ident(v.name, f'{e.name}.{v.name}')}: Self = Self({v.number});")
        self.w("}")
        self.w()

    def oneof_enum(self, m, o):
        lt = "<'a>" if m.name in self.borrowing else ""
        name = oneof_type(m, o)
        self.w("#[derive(Clone, Debug)]")
        self.w(f"pub enum {name}{lt} {{")
        for f in o.fields:
            self.w(f"    {to_camel(f.name)}({self.member_type(f)}),")
        self.w("}")
        self.w()
        self.w(f"impl{lt} {name}{lt} {{")
        self.w("    pub fn number(&self) -> u32 {")
        self.w("        match self {")
        for f in o.fields:
            self.w(f"            Self::{to_camel(f.name)}(_) => {f.number},")
        self.w("        }")
        self.w("    }")
        self.w()
        self.w("    fn encoded_len(&self) -> usize {")
        self.w("        match self {")
        for f in o.fields:
            self.w(f"            Self::{to_camel(f.name)}(v) => {self.len_expr(f, 'v', deref=True)},")
        self.w("        }")
        self.w("    }")
        self.w()
        self.w("    fn encode_fields(&self, w: &mut pb::Writer<'_>) {")
        self.w("        match self {")
        for f in o.fields:
            self.w(f"            Self::{to_camel(f.name)}(v) => {self.write_stmt(f, 'v', deref=True)},")
        self.w("        }")
        self.w("    }")
        self.w("}")
        self.w()

    def len_expr(self, f, v, deref=False):
        n = f.number
        if f.is_message:
            if f.oneof:
                return f"pb::len_lazy({n}, {v})"
            return f"pb::len_message({n}, &**{v})"
        if f.type_name == "bytes":
            return f"pb::len_bytes({n}, {v}.len())"
        return f"pb::len_varint({n}, {self.scalar_expr(f, self.deref(f, v, deref))})"

    def write_stmt(self, f, v, deref=False):
        n = f.number
        if f.is_message:
            if f.oneof:
                return f"w.lazy({n}, {v})"
            return f"w.message({n}, &**{v})"
        if f.type_name == "bytes":
            return f"w.bytes({n}, {v})"
        return f"w.varint({n}, {self.scalar_expr(f, self.deref(f, v, deref))})"

    def deref(self, f, v, deref):
        return f"*{v}" if deref and not f.is_enum else v

    def message(self, m):
        name = to_camel(m.name)
        lt = "<'a>" if m.name in self.borrowing else ""
        any_lt = "<'_>" if m.name in self.borrowing else ""
        fields = sorted(m.fields, key=lambda f: f.number)
        seen = set()

        self.w("#[derive(Clone, Default, Debug)]")
        self.w(f"pub struct {name}{lt} {{")
        for f in m.fields:
            if f.oneof:
                if f.oneof.name not in seen:
                    seen.add(f.oneof.name)
                    olt = "<'a>" if m.name in self.borrowing else ""
                    self.w(f"    pub {ident(f.oneof.name, m.name)}: Option<{oneof_type(m, f.oneof)}{olt}>,")
                continue
            self.w(f"    pub {ident(f.name, f'{m.name}.{f.name}')}: {self.member_type(f)},")
        self.w("}")
        self.w()

        self.w(f"impl pb::Encode for {name}{any_lt} {{")
        self.w("    fn encoded_len(&self) -> usize {")
        if fields:
            self.w("        let mut n = 0;")
            for f, member in self.walk(m, fields):
                self.w(f"        {self.len_stmt(f, member)}")
            self.w("        n")
        else:
            self.w("        0")
        self.w("    }")
        self.w()
        self.w("    fn encode_fields(&self, w: &mut pb::Writer<'_>) {")
        if not fields:
            self.w("        let _ = w;")
        for f, member in self.walk(m, fields):
            self.w(f"        {self.encode_stmt(f, member)}")
        self.w("    }")
        self.w("}")
        self.w()

        lt_a = "'a"
        self.w(f"impl<{lt_a}> pb::Message<{lt_a}> for {name}{lt} {{")
        self.w(f"    fn merge_field(&mut self, field: pb::Field<{lt_a}>) -> Result<(), pb::DecodeError> {{")
        if fields:
            self.w("        match field.number {")
            for f in fields:
                self.w(f"            {f.number} => {self.decode_stmt(m, f)},")
            self.w("            _ => {}")
            self.w("        }")
        else:
            self.w("        let _ = field;")
        self.w("        Ok(())")
        self.w("    }")
        self.w("}")
        self.w()

    def walk(self, m, fields):
        """Fields in number order, a oneof once at its first member."""
        done = set()
        for f in fields:
            if f.oneof:
                if f.oneof.name in done:
                    continue
                done.add(f.oneof.name)
                yield f, f"self.{ident(f.oneof.name, m.name)}"
            else:
                yield f, f"self.{ident(f.name, f'{m.name}.{f.name}')}"

    def len_stmt(self, f, member):
        if f.oneof:
            return f"if let Some(o) = &{member} {{ n += o.encoded_len(); }}"
        if f.repeated:
            if f.is_message:
                return f"n += {member}.iter().map(|v| pb::len_message({f.number}, v)).sum::<usize>();"
            if f.type_name == "bytes":
                return f"n += {member}.iter().map(|v| pb::len_bytes({f.number}, v.len())).sum::<usize>();"
            return f"n += pb::len_packed({f.number}, &{member}, {self.to_wire(f)});"
        if f.is_message:
            return f"if let Some(v) = &{member} {{ n += pb::len_message({f.number}, &**v); }}"
        return f"if {self.is_default(f, member)} {{ n += {self.len_expr(f, member)}; }}"

    def encode_stmt(self, f, member):
        if f.oneof:
            return f"if let Some(o) = &{member} {{ o.encode_fields(w); }}"
        if f.repeated:
            if f.is_message:
                return f"for v in &{member} {{ w.message({f.number}, v); }}"
            if f.type_name == "bytes":
                return f"for v in &{member} {{ w.bytes({f.number}, v); }}"
            return f"w.packed({f.number}, &{member}, {self.to_wire(f)});"
        if f.is_message:
            return f"if let Some(v) = &{member} {{ w.message({f.number}, &**v); }}"
        return f"if {self.is_default(f, member)} {{ {self.write_stmt(f, member)}; }}"

    def read_expr(self, f):
        if f.is_message:
            return "field.lazy()?" if f.oneof else "field.message()?"
        if f.is_enum:
            return f"{to_camel(f.type.name)}(field.int32()?)"
        if f.type_name == "bytes":
            return "field.bytes()?"
        return f"field.{SCALARS[f.type_name][1]}()?"

    def decode_stmt(self, m, f):
        if f.oneof:
            member = ident(f.oneof.name, m.name)
            return f"self.{member} = Some({oneof_type(m, f.oneof)}::{to_camel(f.name)}({self.read_expr(f)}))"
        member = ident(f.name, f"{m.name}.{f.name}")
        if f.repeated:
            if f.is_message or f.type_name == "bytes":
                return f"self.{member}.push({self.read_expr(f)})"
            return f"field.repeated(&mut self.{member}, {self.from_wire(f)})?"
        if f.is_message:
            return f"self.{member} = Some(Box::new({self.read_expr(f)}))"
        return f"self.{member} = {self.read_expr(f)}"

    def generate(self):
        self.w("// Generated by tools/protogen. DO NOT EDIT!")
        self.w(f"// Generated from: {self.proto.name}")
        self.w()
        self.w("pub mod pb {")
        self.w("    include!(\"pb.rs\");")
        self.w("}")
        self.w()
        fields = [f for m in self.proto.messages for f in m.fields]
        if any(f.is_message and not f.repeated and not f.oneof for f in fields):
            self.w("use alloc::boxed::Box;")
        if any(f.repeated for f in fields):
            self.w("use alloc::vec::Vec;")
        self.w()
        for e in self.proto.enums:
            self.enum(e)
        for m in self.proto.messages:
            for o in m.oneofs:
                self.oneof_enum(m, o)
            self.message(m)
        return "".join(self.out)


def generate(proto):
    check(proto)
    stem = proto.name.removesuffix(".proto")
    return {f"{stem}.pb.rs": Gen(proto).generate(), "pb.rs": RUNTIME.read_text(encoding="utf-8")}
