"""C identifier mangling shared by the C generators (protoc-c compatible)."""


def to_camel(name):
    out = []
    upper = True
    for ch in name:
        if ch == "_":
            upper = True
        elif upper:
            out.append(ch.upper())
            upper = False
        else:
            out.append(ch)
    return "".join(out)


def camel_to(name, upper):
    out = []
    was_upper = True
    for ch in name:
        is_upper = ch.isupper()
        if is_upper and not was_upper:
            out.append("_")
        out.append(ch.upper() if upper else ch.lower())
        was_upper = is_upper
    return "".join(out)


def c_name(t):
    return to_camel(t.name)


def lc_name(t):
    return camel_to(t.name, upper=False)


def uc_name(t):
    return camel_to(t.name, upper=True)


def filename_identifier(name):
    return "".join(ch if ch.isalnum() else f"_{ord(ch):02x}" for ch in name)


def enum_value_c_name(enum, value):
    return f"{uc_name(enum)}__{value.name}"


def oneof_case_type(message, oneof):
    return f"{c_name(message)}__{to_camel(oneof.name)}Case"


def oneof_case_prefix(message, oneof):
    return f"{uc_name(message)}__{oneof.name.upper()}"


def struct_members(message):
    seen = set()
    for field in message.fields:
        if field.oneof is None:
            yield field, None
        elif field.oneof.name not in seen:
            seen.add(field.oneof.name)
            yield None, field.oneof
