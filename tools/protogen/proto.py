"""proto3 schema model and parser shared by the protogen generators.

Top-level enums and messages with scalar/enum/message fields, repeated and
oneof; any other construct is rejected rather than ignored.
"""

import re
from pathlib import Path

SCALAR_TYPES = {
    "double", "float", "int32", "int64", "uint32", "uint64", "sint32", "sint64",
    "fixed32", "fixed64", "sfixed32", "sfixed64", "bool", "string", "bytes",
}

TOKEN_RE = re.compile(r"""
    (?P<ws>\s+)
  | (?P<comment>//[^\n]*|/\*.*?\*/)
  | (?P<string>"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')
  | (?P<number>-?(?:0[xX][0-9a-fA-F]+|[0-9]+))
  | (?P<ident>[A-Za-z_][A-Za-z0-9_.]*)
  | (?P<symbol>[{}=;\[\]<>(),])
""", re.X | re.S)


class ProtoError(Exception):
    pass


class EnumValue:
    def __init__(self, name, number):
        self.name = name
        self.number = number


class Enum:
    def __init__(self, name):
        self.name = name
        self.values = []


class Oneof:
    def __init__(self, name):
        self.name = name
        self.fields = []


class Field:
    def __init__(self, name, number, type_name, repeated, oneof):
        self.name = name
        self.number = number
        self.type_name = type_name
        self.repeated = repeated
        self.oneof = oneof
        # Enum or Message once resolved; None for scalars.
        self.type = None

    @property
    def is_scalar(self):
        return self.type_name in SCALAR_TYPES

    @property
    def is_enum(self):
        return isinstance(self.type, Enum)

    @property
    def is_message(self):
        return isinstance(self.type, Message)


class Message:
    def __init__(self, name):
        self.name = name
        self.fields = []
        self.oneofs = []


class ProtoFile:
    def __init__(self, name):
        self.name = name
        self.enums = []
        self.messages = []


class Parser:
    def __init__(self, path, text):
        self.path = path
        self.tokens = []
        line = 1
        pos = 0
        while pos < len(text):
            m = TOKEN_RE.match(text, pos)
            if not m:
                raise ProtoError(f"{path}:{line}: unexpected character {text[pos]!r}")
            kind = m.lastgroup
            if kind not in ("ws", "comment"):
                self.tokens.append((kind, m.group(), line))
            line += m.group().count("\n")
            pos = m.end()
        self.pos = 0

    def error(self, message):
        line = self.tokens[min(self.pos, len(self.tokens) - 1)][2] if self.tokens else 1
        return ProtoError(f"{self.path}:{line}: {message}")

    def peek(self):
        return self.tokens[self.pos][1] if self.pos < len(self.tokens) else None

    def next(self, kind=None):
        if self.pos >= len(self.tokens):
            raise self.error("unexpected end of file")
        tok_kind, text, _ = self.tokens[self.pos]
        if kind and tok_kind != kind:
            raise self.error(f"expected {kind}, got {text!r}")
        self.pos += 1
        return text

    def expect(self, symbol):
        text = self.next()
        if text != symbol:
            self.pos -= 1
            raise self.error(f"expected {symbol!r}, got {text!r}")

    def name(self):
        text = self.next("ident")
        if "." in text:
            self.pos -= 1
            raise self.error(f"qualified name {text!r} is not supported")
        return text

    def number(self):
        return int(self.next("number"), 0)

    def parse_file(self, proto):
        syntax = None
        while self.peek() is not None:
            word = self.next()
            if word == ";":
                continue
            if word == "syntax":
                self.expect("=")
                syntax = self.next("string")[1:-1]
                self.expect(";")
                if syntax != "proto3":
                    raise self.error(f"syntax {syntax!r} is not supported")
            elif word == "enum":
                proto.enums.append(self.parse_enum())
            elif word == "message":
                proto.messages.append(self.parse_message())
            else:
                self.pos -= 1
                raise self.error(f"{word!r} is not supported")
        if syntax is None:
            raise ProtoError(f"{self.path}: missing syntax = \"proto3\"")

    def parse_enum(self):
        enum = Enum(self.name())
        self.expect("{")
        while self.peek() != "}":
            if self.peek() == ";":
                self.next()
                continue
            name = self.name()
            if name in ("option", "reserved"):
                self.pos -= 1
                raise self.error(f"{name!r} in enum is not supported")
            self.expect("=")
            enum.values.append(EnumValue(name, self.number()))
            if self.peek() == "[":
                raise self.error("enum value options are not supported")
            self.expect(";")
        self.expect("}")
        return enum

    def parse_message(self):
        message = Message(self.name())
        self.expect("{")
        while self.peek() != "}":
            word = self.peek()
            if word == ";":
                self.next()
            elif word == "oneof":
                self.next()
                oneof = Oneof(self.name())
                message.oneofs.append(oneof)
                self.expect("{")
                while self.peek() != "}":
                    if self.peek() == ";":
                        self.next()
                        continue
                    field = self.parse_field(oneof, repeated=False)
                    oneof.fields.append(field)
                    message.fields.append(field)
                self.expect("}")
            elif word in ("message", "enum", "map", "optional", "required",
                          "option", "reserved", "extensions", "extend", "group"):
                raise self.error(f"{word!r} in message is not supported")
            else:
                repeated = word == "repeated"
                if repeated:
                    self.next()
                message.fields.append(self.parse_field(None, repeated))
        self.expect("}")
        return message

    def parse_field(self, oneof, repeated):
        type_name = self.name()
        name = self.name()
        self.expect("=")
        number = self.number()
        if self.peek() == "[":
            raise self.error("field options are not supported")
        self.expect(";")
        return Field(name, number, type_name, repeated, oneof)


def resolve(proto, path):
    types = {}
    for t in proto.enums + proto.messages:
        if t.name in types:
            raise ProtoError(f"{path}: duplicate type {t.name!r}")
        types[t.name] = t

    for enum in proto.enums:
        where = f"{path}: enum {enum.name}"
        if not enum.values:
            raise ProtoError(f"{where}: no values")
        if enum.values[0].number != 0:
            raise ProtoError(f"{where}: first value must be 0")
        names = set()
        numbers = set()
        for v in enum.values:
            if v.name in names:
                raise ProtoError(f"{where}: duplicate value {v.name!r}")
            if v.number in numbers:
                raise ProtoError(f"{where}: aliased number {v.number} is not supported")
            names.add(v.name)
            numbers.add(v.number)

    for message in proto.messages:
        where = f"{path}: message {message.name}"
        names = set()
        numbers = set()
        for f in message.fields:
            if f.name in names:
                raise ProtoError(f"{where}: duplicate field {f.name!r}")
            if f.number in numbers:
                raise ProtoError(f"{where}: duplicate field number {f.number}")
            if not 1 <= f.number <= 536870911 or 19000 <= f.number <= 19999:
                raise ProtoError(f"{where}: invalid field number {f.number}")
            names.add(f.name)
            numbers.add(f.number)
            if not f.is_scalar:
                f.type = types.get(f.type_name)
                if f.type is None:
                    raise ProtoError(f"{where}: unknown type {f.type_name!r}")


def parse(path):
    path = Path(path)
    proto = ProtoFile(path.name)
    Parser(path, path.read_text(encoding="utf-8")).parse_file(proto)
    resolve(proto, path)
    return proto
