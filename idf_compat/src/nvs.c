// Host implementation of the ESP-IDF NVS C API (nvs.h / nvs_flash.h), backed by
// a TOML-like text file. Format and type-matching rules: idf_compat/README.md.
//
// Fidelity notes: writes are persisted eagerly (every set), so nvs_commit() is
// effectively a flush. The file is read once; edits made to it while the process
// runs are overwritten by the next write.

#include "nvs_flash.h"
#include "idf_compat_internal.h"

#include <inttypes.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// ---------------------------------------------------------------------------
// In-memory store: the file as a list of lines, so untouched lines (comments,
// blank lines, unparsable lines) are written back verbatim.
// ---------------------------------------------------------------------------
typedef enum {
    T_NONE,
    T_U8, T_I8, T_U16, T_I16, T_U32, T_I32, T_U64, T_I64,
    T_STR, T_BLOB,
} vtype_t;

typedef enum { LIT_INT, LIT_STR, LIT_BLOB } lit_t;

typedef enum { LINE_RAW, LINE_SECTION, LINE_ENTRY } line_kind_t;

typedef struct {
    line_kind_t kind;
    char *text;
    char *name;
    size_t key_end;     // entry: text[0, key_end) is indent + key as written
    size_t value_end;   // entry: text[value_end, ...) is trailing space + comment
    vtype_t type;
    lit_t lit;
    bool neg;
    uint64_t mag;
    uint8_t *data;      // str (without NUL) / blob bytes
    size_t len;
} line_t;

typedef struct {
    nvs_handle_t handle;
    char *ns;
    bool readonly;
    bool in_use;
} open_handle_t;

static const struct {
    const char *name;
    bool is_signed;
    int bits;
} k_types[] = {
    [T_NONE] = {"", false, 0},
    [T_U8] = {"u8", false, 8},    [T_I8] = {"i8", true, 8},
    [T_U16] = {"u16", false, 16}, [T_I16] = {"i16", true, 16},
    [T_U32] = {"u32", false, 32}, [T_I32] = {"i32", true, 32},
    [T_U64] = {"u64", false, 64}, [T_I64] = {"i64", true, 64},
    [T_STR] = {"str", false, 0},  [T_BLOB] = {"blob", false, 0},
};

static pthread_mutex_t s_mutex = PTHREAD_MUTEX_INITIALIZER;
static char s_path[1024] = "nvs_data.toml";
static bool s_loaded = false;

static line_t *s_lines;
static size_t s_line_count, s_line_cap;

static open_handle_t *s_handles;
static size_t s_handle_count, s_handle_cap;
static nvs_handle_t s_next_handle = 1;

#define LOCK()   pthread_mutex_lock(&s_mutex)
#define UNLOCK() pthread_mutex_unlock(&s_mutex)

// ---------------------------------------------------------------------------
// String builder
// ---------------------------------------------------------------------------
typedef struct {
    char *buf;
    size_t len, cap;
} sb_t;

static void sb_putn(sb_t *sb, const char *s, size_t n) {
    if (sb->len + n + 1 > sb->cap) {
        size_t ncap = sb->cap ? sb->cap : 64;
        while (sb->len + n + 1 > ncap) ncap *= 2;
        sb->buf = realloc(sb->buf, ncap);
        sb->cap = ncap;
    }
    memcpy(sb->buf + sb->len, s, n);
    sb->len += n;
    sb->buf[sb->len] = '\0';
}

static void sb_puts(sb_t *sb, const char *s) { sb_putn(sb, s, strlen(s)); }

static void sb_printf(sb_t *sb, const char *fmt, ...) {
    char tmp[64];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(tmp, sizeof(tmp), fmt, ap);
    va_end(ap);
    sb_putn(sb, tmp, (size_t)n);
}

static char *sb_take(sb_t *sb) {
    if (!sb->buf) return strdup("");
    return sb->buf;
}

// ---------------------------------------------------------------------------
// Value formatting
// ---------------------------------------------------------------------------
static bool is_bare_key(const char *s) {
    if (!*s) return false;
    for (; *s; s++) {
        char c = *s;
        if (!((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ||
              c == '_' || c == '-'))
            return false;
    }
    return true;
}

static void put_quoted(sb_t *sb, const uint8_t *s, size_t n) {
    sb_putn(sb, "\"", 1);
    for (size_t i = 0; i < n; i++) {
        uint8_t c = s[i];
        if (c == '"') sb_puts(sb, "\\\"");
        else if (c == '\\') sb_puts(sb, "\\\\");
        else if (c == '\n') sb_puts(sb, "\\n");
        else if (c == '\t') sb_puts(sb, "\\t");
        else if (c < 0x20 || c == 0x7f) sb_printf(sb, "\\u%04x", c);
        else sb_putn(sb, (const char *)&c, 1);
    }
    sb_putn(sb, "\"", 1);
}

static void put_name(sb_t *sb, const char *name) {
    if (is_bare_key(name)) sb_puts(sb, name);
    else put_quoted(sb, (const uint8_t *)name, strlen(name));
}

static void put_value(sb_t *sb, const line_t *l) {
    switch (l->lit) {
    case LIT_INT:
        sb_printf(sb, "%s%" PRIu64, l->neg ? "-" : "", l->mag);
        break;
    case LIT_STR:
        put_quoted(sb, l->data, l->len);
        break;
    case LIT_BLOB:
        sb_puts(sb, "0x");
        for (size_t i = 0; i < l->len; i++) sb_printf(sb, "%02x", l->data[i]);
        break;
    }
}

static void entry_render(line_t *l) {
    sb_t sb = {0};
    sb_putn(&sb, l->text, l->key_end);
    if (l->type != T_NONE) {
        sb_puts(&sb, ": ");
        sb_puts(&sb, k_types[l->type].name);
    }
    sb_puts(&sb, " = ");
    put_value(&sb, l);
    size_t value_end = sb.len;
    sb_puts(&sb, l->text + l->value_end);
    free(l->text);
    l->text = sb_take(&sb);
    l->value_end = value_end;
}

// ---------------------------------------------------------------------------
// Line parsing
// ---------------------------------------------------------------------------
static int hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static const char *skip_ws(const char *p) {
    while (*p == ' ' || *p == '\t') p++;
    return p;
}

static bool rest_is_blank(const char *p) {
    p = skip_ws(p);
    return *p == '\0' || *p == '#';
}

static void put_utf8(sb_t *sb, uint32_t cp) {
    char b[4];
    size_t n;
    if (cp < 0x80) { b[0] = (char)cp; n = 1; }
    else if (cp < 0x800) { b[0] = (char)(0xc0 | cp >> 6); b[1] = (char)(0x80 | (cp & 0x3f)); n = 2; }
    else if (cp < 0x10000) {
        b[0] = (char)(0xe0 | cp >> 12); b[1] = (char)(0x80 | ((cp >> 6) & 0x3f));
        b[2] = (char)(0x80 | (cp & 0x3f)); n = 3;
    } else {
        b[0] = (char)(0xf0 | cp >> 18); b[1] = (char)(0x80 | ((cp >> 12) & 0x3f));
        b[2] = (char)(0x80 | ((cp >> 6) & 0x3f)); b[3] = (char)(0x80 | (cp & 0x3f)); n = 4;
    }
    sb_putn(sb, b, n);
}

// NUL is rejected: neither an NVS string nor a key can hold one.
static const char *parse_string(const char **pp, sb_t *out) {
    const char *p = *pp + 1;
    for (;;) {
        char c = *p;
        if (c == '\0') return "unterminated string";
        if (c == '"') break;
        if (c != '\\') { sb_putn(out, p++, 1); continue; }
        p++;
        switch (*p) {
        case '"': sb_putn(out, "\"", 1); p++; break;
        case '\\': sb_putn(out, "\\", 1); p++; break;
        case 'n': sb_putn(out, "\n", 1); p++; break;
        case 't': sb_putn(out, "\t", 1); p++; break;
        case 'r': sb_putn(out, "\r", 1); p++; break;
        case 'b': sb_putn(out, "\b", 1); p++; break;
        case 'f': sb_putn(out, "\f", 1); p++; break;
        case 'u':
        case 'U': {
            int digits = *p == 'u' ? 4 : 8;
            uint32_t cp = 0;
            p++;
            for (int i = 0; i < digits; i++, p++) {
                int v = hexval(*p);
                if (v < 0) return "bad \\u escape";
                cp = cp << 4 | (uint32_t)v;
            }
            if (cp == 0 || cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) return "bad \\u escape";
            put_utf8(out, cp);
            break;
        }
        default:
            return "bad escape";
        }
    }
    *pp = p + 1;
    return NULL;
}

static const char *parse_name(const char **pp, char **out) {
    const char *p = *pp;
    if (*p == '"') {
        sb_t sb = {0};
        const char *err = parse_string(&p, &sb);
        if (err) { free(sb.buf); return err; }
        *out = sb_take(&sb);
    } else {
        const char *start = p;
        while ((*p >= 'A' && *p <= 'Z') || (*p >= 'a' && *p <= 'z') || (*p >= '0' && *p <= '9') ||
               *p == '_' || *p == '-')
            p++;
        if (p == start) return "missing name";
        *out = strndup(start, (size_t)(p - start));
    }
    *pp = p;
    return NULL;
}

static const char *parse_value(const char **pp, line_t *l) {
    const char *p = *pp;
    if (*p == '"') {
        sb_t sb = {0};
        const char *err = parse_string(&p, &sb);
        if (err) { free(sb.buf); return err; }
        l->lit = LIT_STR;
        l->len = sb.len;
        l->data = (uint8_t *)sb.buf;
    } else if (p[0] == '0' && (p[1] == 'x' || p[1] == 'X')) {
        p += 2;
        const char *start = p;
        while (hexval(*p) >= 0) p++;
        size_t digits = (size_t)(p - start);
        if (digits % 2) return "odd number of hex digits";
        l->lit = LIT_BLOB;
        l->len = digits / 2;
        l->data = l->len ? malloc(l->len) : NULL;
        for (size_t i = 0; i < l->len; i++)
            l->data[i] = (uint8_t)(hexval(start[2 * i]) << 4 | hexval(start[2 * i + 1]));
    } else {
        l->lit = LIT_INT;
        l->neg = *p == '-';
        if (l->neg) p++;
        if (*p < '0' || *p > '9') return "bad value";
        l->mag = 0;
        for (; *p >= '0' && *p <= '9'; p++) {
            uint64_t d = (uint64_t)(*p - '0');
            if (l->mag > (UINT64_MAX - d) / 10) return "integer out of range";
            l->mag = l->mag * 10 + d;
        }
        if (l->neg && l->mag > (uint64_t)INT64_MAX + 1) return "integer out of range";
        if (l->mag == 0) l->neg = false;
    }
    *pp = p;
    return NULL;
}

static bool int_fits(const line_t *l, vtype_t t) {
    int bits = k_types[t].bits;
    if (k_types[t].is_signed) {
        uint64_t limit = (uint64_t)1 << (bits - 1);
        return l->neg ? l->mag <= limit : l->mag < limit;
    }
    if (l->neg) return false;
    return bits == 64 || l->mag < ((uint64_t)1 << bits);
}

static const char *check_type(const line_t *l) {
    switch (l->type) {
    case T_NONE: return NULL;
    case T_STR: return l->lit == LIT_STR ? NULL : "str needs a \"...\" value";
    case T_BLOB: return l->lit == LIT_BLOB ? NULL : "blob needs a 0x... value";
    default:
        if (l->lit != LIT_INT) return "integer type needs a decimal value";
        return int_fits(l, l->type) ? NULL : "integer out of range for its type";
    }
}

static const char *parse_line(line_t *l) {
    const char *text = l->text;
    const char *p = skip_ws(text);
    const char *err;
    if (*p == '\0' || *p == '#') return NULL;

    if (*p == '[') {
        p = skip_ws(p + 1);
        if ((err = parse_name(&p, &l->name))) return err;
        p = skip_ws(p);
        if (*p != ']') return "expected ']'";
        if (!rest_is_blank(p + 1)) return "trailing characters";
        l->kind = LINE_SECTION;
        return NULL;
    }

    if ((err = parse_name(&p, &l->name))) return err;
    l->key_end = (size_t)(p - text);
    p = skip_ws(p);
    l->type = T_NONE;
    if (*p == ':') {
        p = skip_ws(p + 1);
        const char *start = p;
        while ((*p >= 'a' && *p <= 'z') || (*p >= '0' && *p <= '9')) p++;
        size_t n = (size_t)(p - start);
        for (int t = T_U8; t <= T_BLOB; t++)
            if (strlen(k_types[t].name) == n && strncmp(k_types[t].name, start, n) == 0)
                l->type = (vtype_t)t;
        if (l->type == T_NONE) return "unknown type";
        p = skip_ws(p);
    }
    if (*p != '=') return "expected '='";
    p = skip_ws(p + 1);
    if ((err = parse_value(&p, l))) return err;
    l->value_end = (size_t)(p - text);
    if (!rest_is_blank(p)) return "trailing characters";
    if ((err = check_type(l))) return err;
    l->kind = LINE_ENTRY;
    return NULL;
}

// ---------------------------------------------------------------------------
// Store helpers (all assume the lock is held)
// ---------------------------------------------------------------------------
static void line_free(line_t *l) {
    free(l->text);
    free(l->name);
    free(l->data);
}

static void line_reset_to_raw(line_t *l) {
    char *text = l->text;
    l->text = NULL;
    line_free(l);
    *l = (line_t){.kind = LINE_RAW, .text = text};
}

static void line_insert(size_t at, line_t l) {
    if (s_line_count == s_line_cap) {
        size_t ncap = s_line_cap ? s_line_cap * 2 : 16;
        s_lines = realloc(s_lines, ncap * sizeof(*s_lines));
        s_line_cap = ncap;
    }
    memmove(&s_lines[at + 1], &s_lines[at], (s_line_count - at) * sizeof(*s_lines));
    s_lines[at] = l;
    s_line_count++;
}

static void store_free(void) {
    for (size_t i = 0; i < s_line_count; i++) line_free(&s_lines[i]);
    free(s_lines);
    s_lines = NULL;
    s_line_count = 0;
    s_line_cap = 0;
}

static bool find_ns(const char *ns) {
    for (size_t i = 0; i < s_line_count; i++)
        if (s_lines[i].kind == LINE_SECTION && strcmp(s_lines[i].name, ns) == 0) return true;
    return false;
}

// The last occurrence of a duplicated key wins.
static line_t *find_entry(const char *ns, const char *key) {
    line_t *found = NULL;
    bool in = false;
    for (size_t i = 0; i < s_line_count; i++) {
        line_t *l = &s_lines[i];
        if (l->kind == LINE_SECTION) in = strcmp(l->name, ns) == 0;
        else if (in && l->kind == LINE_ENTRY && strcmp(l->name, key) == 0) found = l;
    }
    return found;
}

static void insert_entry(const char *ns, line_t entry) {
    size_t at = 0;
    bool found = false, in = false;
    for (size_t i = 0; i < s_line_count; i++) {
        line_t *l = &s_lines[i];
        if (l->kind == LINE_SECTION) in = strcmp(l->name, ns) == 0;
        if (in && l->kind != LINE_RAW) { at = i + 1; found = true; }
    }
    if (!found) {
        if (s_line_count && s_lines[s_line_count - 1].text[0] != '\0')
            line_insert(s_line_count, (line_t){.kind = LINE_RAW, .text = strdup("")});
        sb_t sb = {0};
        sb_puts(&sb, "[");
        put_name(&sb, ns);
        sb_puts(&sb, "]");
        line_insert(s_line_count, (line_t){.kind = LINE_SECTION, .text = sb_take(&sb), .name = strdup(ns)});
        at = s_line_count;
    }
    line_insert(at, entry);
}

// Removes entry lines in `ns` (all of them when key is NULL). Returns the count.
static size_t remove_entries(const char *ns, const char *key) {
    size_t kept = 0, removed = 0;
    bool in = false;
    for (size_t i = 0; i < s_line_count; i++) {
        line_t *l = &s_lines[i];
        if (l->kind == LINE_SECTION) in = strcmp(l->name, ns) == 0;
        if (in && l->kind == LINE_ENTRY && (!key || strcmp(l->name, key) == 0)) {
            line_free(l);
            removed++;
        } else {
            s_lines[kept++] = *l;
        }
    }
    s_line_count = kept;
    return removed;
}

/* Simulator hardware identity is analogous to eFuse, not application NVS, so
 * preserve its reserved namespace when the emulated NVS partition is erased. */
static void store_free_except_simulator_identity(void) {
    size_t kept = 0;
    bool drop = false;
    for (size_t i = 0; i < s_line_count; i++) {
        line_t *l = &s_lines[i];
        if (l->kind == LINE_SECTION) drop = strcmp(l->name, IDF_COMPAT_SIM_NVS_NAMESPACE) != 0;
        if (drop) line_free(l);
        else s_lines[kept++] = *l;
    }
    s_line_count = kept;
}

static open_handle_t *find_handle(nvs_handle_t handle) {
    for (size_t i = 0; i < s_handle_count; i++)
        if (s_handles[i].in_use && s_handles[i].handle == handle) return &s_handles[i];
    return NULL;
}

// ---------------------------------------------------------------------------
// Persistence (assume the lock is held)
// ---------------------------------------------------------------------------
static void warn_line(size_t lineno, const char *msg) {
    fprintf(stderr, "W (nvs) %s:%zu: %s; line ignored\n", s_path, lineno, msg);
}

static void ensure_loaded(void) {
    if (s_loaded) return;
    s_loaded = true;

    FILE *f = fopen(s_path, "rb");
    if (!f) return;
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz <= 0) {
        fclose(f);
        return;
    }
    char *buf = malloc((size_t)sz + 1);
    size_t got = fread(buf, 1, (size_t)sz, f);
    buf[got] = '\0';
    fclose(f);

    const char *section = NULL;
    size_t lineno = 0;
    for (char *p = buf; *p;) {
        char *nl = strchr(p, '\n');
        size_t n = nl ? (size_t)(nl - p) : strlen(p);
        if (n && p[n - 1] == '\r') n--;
        lineno++;

        line_t l = {.kind = LINE_RAW, .text = strndup(p, n)};
        const char *err = parse_line(&l);
        if (!err && l.kind == LINE_ENTRY && !section) err = "entry outside a [namespace]";
        if (err) {
            warn_line(lineno, err);
            line_reset_to_raw(&l);
        } else if (l.kind == LINE_ENTRY && find_entry(section, l.name)) {
            fprintf(stderr, "W (nvs) %s:%zu: duplicate key '%s' overrides an earlier line\n",
                    s_path, lineno, l.name);
        }
        line_insert(s_line_count, l);
        if (l.kind == LINE_SECTION) section = s_lines[s_line_count - 1].name;

        if (!nl) break;
        p = nl + 1;
    }
    free(buf);
}

static esp_err_t save_unlocked(void) {
    FILE *f = fopen(s_path, "wb");
    if (!f) return ESP_FAIL;
    bool write_failed = false;
    for (size_t i = 0; i < s_line_count; i++)
        if (fputs(s_lines[i].text, f) == EOF || fputc('\n', f) == EOF) write_failed = true;
    bool close_failed = fclose(f) != 0;
    return write_failed || close_failed ? ESP_FAIL : ESP_OK;
}

// ---------------------------------------------------------------------------
// Typed access helpers (acquire the lock)
// ---------------------------------------------------------------------------
static esp_err_t set_value(nvs_handle_t handle, const char *key, const line_t *value) {
    esp_err_t ret;
    open_handle_t *oh;
    line_t *l;
    LOCK();
    ensure_loaded();
    oh = find_handle(handle);
    if (!oh) { ret = ESP_ERR_NVS_INVALID_HANDLE; goto done; }
    if (oh->readonly) { ret = ESP_ERR_NVS_READ_ONLY; goto done; }
    l = find_entry(oh->ns, key);
    if (l) {
        free(l->data);
    } else {
        sb_t sb = {0};
        put_name(&sb, key);
        line_t entry = {.kind = LINE_ENTRY, .name = strdup(key), .key_end = sb.len, .value_end = sb.len};
        entry.text = sb_take(&sb);
        insert_entry(oh->ns, entry);
        l = find_entry(oh->ns, key);
    }
    l->type = value->type;
    l->lit = value->lit;
    l->neg = value->neg;
    l->mag = value->mag;
    l->len = value->len;
    l->data = NULL;
    if (value->len) {
        l->data = malloc(value->len);
        memcpy(l->data, value->data, value->len);
    }
    entry_render(l);
    ret = save_unlocked();
done:
    UNLOCK();
    return ret;
}

static esp_err_t set_signed(nvs_handle_t h, const char *key, vtype_t type, int64_t v) {
    line_t value = {.type = type, .lit = LIT_INT, .neg = v < 0, .mag = v < 0 ? 0 - (uint64_t)v : (uint64_t)v};
    return set_value(h, key, &value);
}

static esp_err_t set_unsigned(nvs_handle_t h, const char *key, vtype_t type, uint64_t v) {
    line_t value = {.type = type, .lit = LIT_INT, .mag = v};
    return set_value(h, key, &value);
}

static esp_err_t set_bytes(nvs_handle_t h, const char *key, vtype_t type, const void *data, size_t len) {
    line_t value = {.type = type, .lit = type == T_STR ? LIT_STR : LIT_BLOB, .data = (uint8_t *)data, .len = len};
    return set_value(h, key, &value);
}

static esp_err_t lookup(nvs_handle_t handle, const char *key, line_t **out) {
    open_handle_t *oh = find_handle(handle);
    if (!oh) return ESP_ERR_NVS_INVALID_HANDLE;
    *out = find_entry(oh->ns, key);
    return *out ? ESP_OK : ESP_ERR_NVS_NOT_FOUND;
}

static esp_err_t get_int(nvs_handle_t handle, const char *key, vtype_t type, void *out) {
    esp_err_t ret;
    line_t *l;
    LOCK();
    ensure_loaded();
    if ((ret = lookup(handle, key, &l)) != ESP_OK) goto done;
    if (l->type == T_NONE ? l->lit != LIT_INT || !int_fits(l, type) : l->type != type) {
        ret = ESP_ERR_NVS_NOT_FOUND;
        goto done;
    }
    uint64_t u = l->neg ? 0 - l->mag : l->mag;
    switch (k_types[type].bits) {
    case 8: *(uint8_t *)out = (uint8_t)u; break;
    case 16: *(uint16_t *)out = (uint16_t)u; break;
    case 32: *(uint32_t *)out = (uint32_t)u; break;
    default: *(uint64_t *)out = u; break;
    }
done:
    UNLOCK();
    return ret;
}

// out == NULL → report required size only (matches nvs_get_str/blob).
static esp_err_t get_var(nvs_handle_t handle, const char *key, vtype_t type, void *out, size_t *length) {
    esp_err_t ret;
    line_t *l;
    size_t size;
    LOCK();
    ensure_loaded();
    if ((ret = lookup(handle, key, &l)) != ESP_OK) goto done;
    bool ok = l->type == T_NONE ? l->lit == LIT_STR || (type == T_BLOB && l->lit == LIT_BLOB) : l->type == type;
    if (!ok) { ret = ESP_ERR_NVS_NOT_FOUND; goto done; }
    size = l->len + (type == T_STR ? 1 : 0);
    if (out == NULL) {
        *length = size;
        goto done;
    }
    if (*length < size) { ret = ESP_ERR_NVS_INVALID_LENGTH; goto done; }
    if (l->len) memcpy(out, l->data, l->len);
    if (type == T_STR) ((char *)out)[l->len] = '\0';
    *length = size;
done:
    UNLOCK();
    return ret;
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------
void nvs_flash_sim_set_path(const char *path) {
    LOCK();
    snprintf(s_path, sizeof(s_path), "%s", path);
    store_free();
    s_loaded = false;
    UNLOCK();
}

esp_err_t nvs_flash_init(void) {
    LOCK();
    ensure_loaded();
    UNLOCK();
    return ESP_OK;
}

esp_err_t nvs_flash_erase(void) {
    LOCK();
    ensure_loaded();
    store_free_except_simulator_identity();
    esp_err_t ret = save_unlocked();
    UNLOCK();
    return ret;
}

void nvs_flash_deregister_security_scheme(void) {
    // Host store is plaintext; no security scheme is ever registered.
}

esp_err_t nvs_open(const char *name, nvs_open_mode_t open_mode, nvs_handle_t *out_handle) {
    esp_err_t ret;
    open_handle_t *slot = NULL;
    LOCK();
    ensure_loaded();
    if (open_mode == NVS_READONLY && !find_ns(name)) {
        ret = ESP_ERR_NVS_NOT_FOUND;
        goto done;
    }
    for (size_t i = 0; i < s_handle_count; i++)
        if (!s_handles[i].in_use) { slot = &s_handles[i]; break; }
    if (!slot) {
        if (s_handle_count == s_handle_cap) {
            size_t ncap = s_handle_cap ? s_handle_cap * 2 : 4;
            open_handle_t *t = realloc(s_handles, ncap * sizeof(*t));
            if (!t) { ret = ESP_ERR_NO_MEM; goto done; }
            s_handles = t;
            s_handle_cap = ncap;
        }
        slot = &s_handles[s_handle_count++];
    }
    slot->handle = s_next_handle++;
    slot->ns = strdup(name);
    slot->readonly = (open_mode == NVS_READONLY);
    slot->in_use = true;
    *out_handle = slot->handle;
    ret = ESP_OK;
done:
    UNLOCK();
    return ret;
}

void nvs_close(nvs_handle_t handle) {
    LOCK();
    open_handle_t *oh = find_handle(handle);
    if (oh) {
        free(oh->ns);
        oh->ns = NULL;
        oh->in_use = false;
    }
    UNLOCK();
}

esp_err_t nvs_commit(nvs_handle_t handle) {
    esp_err_t ret;
    LOCK();
    if (!find_handle(handle)) { ret = ESP_ERR_NVS_INVALID_HANDLE; goto done; }
    ret = save_unlocked();
done:
    UNLOCK();
    return ret;
}

esp_err_t nvs_erase_key(nvs_handle_t handle, const char *key) {
    esp_err_t ret;
    open_handle_t *oh;
    LOCK();
    ensure_loaded();
    oh = find_handle(handle);
    if (!oh) { ret = ESP_ERR_NVS_INVALID_HANDLE; goto done; }
    if (oh->readonly) { ret = ESP_ERR_NVS_READ_ONLY; goto done; }
    if (remove_entries(oh->ns, key) == 0) { ret = ESP_ERR_NVS_NOT_FOUND; goto done; }
    ret = save_unlocked();
done:
    UNLOCK();
    return ret;
}

esp_err_t nvs_erase_all(nvs_handle_t handle) {
    esp_err_t ret;
    open_handle_t *oh;
    LOCK();
    ensure_loaded();
    oh = find_handle(handle);
    if (!oh) { ret = ESP_ERR_NVS_INVALID_HANDLE; goto done; }
    if (oh->readonly) { ret = ESP_ERR_NVS_READ_ONLY; goto done; }
    remove_entries(oh->ns, NULL);
    ret = save_unlocked();
done:
    UNLOCK();
    return ret;
}

// Purge erased key-value pairs. The file store deletes entries outright (no
// tombstones), so there is nothing to reclaim; just validate the handle.
esp_err_t nvs_purge_all(nvs_handle_t handle) {
    esp_err_t ret;
    open_handle_t *oh;
    LOCK();
    ensure_loaded();
    oh = find_handle(handle);
    if (!oh) { ret = ESP_ERR_NVS_INVALID_HANDLE; goto done; }
    if (oh->readonly) { ret = ESP_ERR_NVS_READ_ONLY; goto done; }
    ret = ESP_OK;
done:
    UNLOCK();
    return ret;
}

esp_err_t nvs_get_used_entry_count(nvs_handle_t handle, size_t *used_entries) {
    esp_err_t ret;
    open_handle_t *oh;
    size_t count = 0;
    bool in = false;
    LOCK();
    ensure_loaded();
    oh = find_handle(handle);
    if (!oh) { ret = ESP_ERR_NVS_INVALID_HANDLE; goto done; }
    for (size_t i = 0; i < s_line_count; i++) {
        line_t *l = &s_lines[i];
        if (l->kind == LINE_SECTION) in = strcmp(l->name, oh->ns) == 0;
        else if (in && l->kind == LINE_ENTRY && find_entry(oh->ns, l->name) == l) count++;
    }
    *used_entries = count;
    ret = ESP_OK;
done:
    UNLOCK();
    return ret;
}

esp_err_t nvs_set_i8(nvs_handle_t h, const char *k, int8_t v)    { return set_signed(h, k, T_I8, v); }
esp_err_t nvs_set_u8(nvs_handle_t h, const char *k, uint8_t v)   { return set_unsigned(h, k, T_U8, v); }
esp_err_t nvs_set_i16(nvs_handle_t h, const char *k, int16_t v)  { return set_signed(h, k, T_I16, v); }
esp_err_t nvs_set_u16(nvs_handle_t h, const char *k, uint16_t v) { return set_unsigned(h, k, T_U16, v); }
esp_err_t nvs_set_i32(nvs_handle_t h, const char *k, int32_t v)  { return set_signed(h, k, T_I32, v); }
esp_err_t nvs_set_u32(nvs_handle_t h, const char *k, uint32_t v) { return set_unsigned(h, k, T_U32, v); }
esp_err_t nvs_set_i64(nvs_handle_t h, const char *k, int64_t v)  { return set_signed(h, k, T_I64, v); }
esp_err_t nvs_set_u64(nvs_handle_t h, const char *k, uint64_t v) { return set_unsigned(h, k, T_U64, v); }
esp_err_t nvs_set_str(nvs_handle_t h, const char *k, const char *v) { return set_bytes(h, k, T_STR, v, strlen(v)); }
esp_err_t nvs_set_blob(nvs_handle_t h, const char *k, const void *v, size_t len) { return set_bytes(h, k, T_BLOB, v, len); }

esp_err_t nvs_get_i8(nvs_handle_t h, const char *k, int8_t *v)    { return get_int(h, k, T_I8, v); }
esp_err_t nvs_get_u8(nvs_handle_t h, const char *k, uint8_t *v)   { return get_int(h, k, T_U8, v); }
esp_err_t nvs_get_i16(nvs_handle_t h, const char *k, int16_t *v)  { return get_int(h, k, T_I16, v); }
esp_err_t nvs_get_u16(nvs_handle_t h, const char *k, uint16_t *v) { return get_int(h, k, T_U16, v); }
esp_err_t nvs_get_i32(nvs_handle_t h, const char *k, int32_t *v)  { return get_int(h, k, T_I32, v); }
esp_err_t nvs_get_u32(nvs_handle_t h, const char *k, uint32_t *v) { return get_int(h, k, T_U32, v); }
esp_err_t nvs_get_i64(nvs_handle_t h, const char *k, int64_t *v)  { return get_int(h, k, T_I64, v); }
esp_err_t nvs_get_u64(nvs_handle_t h, const char *k, uint64_t *v) { return get_int(h, k, T_U64, v); }
esp_err_t nvs_get_str(nvs_handle_t h, const char *k, char *v, size_t *len)  { return get_var(h, k, T_STR, v, len); }
esp_err_t nvs_get_blob(nvs_handle_t h, const char *k, void *v, size_t *len) { return get_var(h, k, T_BLOB, v, len); }
