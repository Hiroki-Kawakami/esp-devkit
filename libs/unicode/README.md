# unicode

Unicode character properties, from the Unicode Character Database version in
`UNICODE_VERSION`.

| function | |
|---|---|
| `east_asian_width` | East_Asian_Width (UAX #11), with the defaults the database gives unassigned code points (`Wide` in the ideograph blocks) |
| `general_category` | General_Category |
| `compose_kana` | the canonical composition of a kana and U+3099/U+309A, the pairs text in NFD carries |
| `is_kana_voicing_mark` | U+3099 and U+309A |

How wide a character is on screen is not a property of its own: a terminal
counts columns from `east_asian_width` and `general_category`, proportional
text from its glyphs.

`src/tables.rs` is generated; `tools/gen_tables.sh` regenerates it from the
database nixpkgs carries (run inside the nix dev shell). Each property is a
sorted list of the code points where its value changes, searched in binary.

## Tests

`rust_test_unicode` in a simulator build that includes the component.
