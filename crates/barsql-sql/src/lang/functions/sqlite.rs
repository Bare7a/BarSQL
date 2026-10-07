// SQLite built-ins. Turso reads these too. Overrides the shared core where SQLite differs.

use super::flags::{NO_PARENS, SYNTAX};
use super::{BuiltinFn, aggregate, scalar, sig, table, window};

pub(super) const PACK: &[BuiltinFn] = &[
    scalar("acos", &[sig("x", "real")], "Arccosine in radians.").since("SQLite 3.35"),
    scalar("acosh", &[sig("x", "real")], "Inverse hyperbolic cosine.").since("SQLite 3.35"),
    scalar("asin", &[sig("x", "real")], "Arcsine in radians.").since("SQLite 3.35"),
    scalar("asinh", &[sig("x", "real")], "Inverse hyperbolic sine.").since("SQLite 3.35"),
    scalar("atan", &[sig("x", "real")], "Arctangent in radians.").since("SQLite 3.35"),
    scalar(
        "atan2",
        &[sig("y, x", "real")],
        "Arctangent of y/x in radians, in the quadrant given by the signs of x and y.",
    )
    .since("SQLite 3.35"),
    scalar("atanh", &[sig("x", "real")], "Inverse hyperbolic tangent.").since("SQLite 3.35"),
    scalar(
        "bm25",
        &[sig("fts_table [, weight, ...]", "real")],
        "Relevance of the current FTS5 match; smaller values mean better matches.",
    ),
    scalar("ceil", &[sig("x", "numeric")], "Smallest integer value not less than the argument.").since("SQLite 3.35"),
    scalar("ceiling", &[sig("x", "numeric")], "Smallest integer value not less than the argument; alias of ceil.")
        .since("SQLite 3.35"),
    scalar("changes", &[sig("", "integer")], "Rows changed by the most recent INSERT, UPDATE or DELETE statement."),
    scalar(
        "char",
        &[sig("code_point [, ...]", "text")],
        "Text made of the characters with the given Unicode code points.",
    ),
    scalar("concat", &[sig("value [, ...]", "text")], "Joins the arguments as text, skipping nulls.")
        .since("SQLite 3.44"),
    scalar(
        "concat_ws",
        &[sig("separator, value [, ...]", "text")],
        "Joins the non-null values after the first, with the first as separator.",
    )
    .since("SQLite 3.44"),
    scalar("cos", &[sig("x", "real")], "Cosine of an angle in radians.").since("SQLite 3.35"),
    scalar("cosh", &[sig("x", "real")], "Hyperbolic cosine.").since("SQLite 3.35"),
    window("cume_dist", &[sig("", "real")], "Fraction of partition rows that sort at or before the current row."),
    scalar("current_date", &[sig("", "text")], "Current UTC date as YYYY-MM-DD.").flags(NO_PARENS | SYNTAX),
    scalar("current_time", &[sig("", "text")], "Current UTC time as HH:MM:SS.").flags(NO_PARENS | SYNTAX),
    scalar("current_timestamp", &[sig("", "text")], "Current UTC date and time as YYYY-MM-DD HH:MM:SS.")
        .flags(NO_PARENS | SYNTAX),
    scalar("date", &[sig("time-value [, modifier, ...]", "text")], "Date as YYYY-MM-DD, after applying the modifiers."),
    scalar(
        "datetime",
        &[sig("time-value [, modifier, ...]", "text")],
        "Date and time as YYYY-MM-DD HH:MM:SS, after applying the modifiers.",
    ),
    scalar("degrees", &[sig("x", "real")], "Converts radians to degrees.").since("SQLite 3.35"),
    scalar("exp", &[sig("x", "real")], "Euler's number e raised to the given power.").since("SQLite 3.35"),
    scalar(
        "format",
        &[sig("format [, value, ...]", "text")],
        "Text built from a printf-style format string and values.",
    )
    .since("SQLite 3.38"),
    scalar(
        "glob",
        &[sig("pattern, text", "integer")],
        "Whether the text matches a case-sensitive GLOB pattern, as 1 or 0.",
    ),
    aggregate(
        "group_concat",
        &[sig("value [, separator]", "text")],
        "Non-null values of the group joined as text, by commas or the separator.",
    ),
    scalar("hex", &[sig("value", "text")], "Uppercase hexadecimal text of a value's bytes."),
    scalar(
        "highlight",
        &[sig("fts_table, column_index, open, close", "text")],
        "Text of an FTS5 column with markup around each phrase match.",
    ),
    scalar(
        "if",
        &[sig("condition, value [, condition, value, ...] [, else]", "any")],
        "Alternative spelling of iif: the value paired with the first true condition.",
    )
    .since("SQLite 3.48"),
    scalar("ifnull", &[sig("value, fallback", "any")], "First argument if it isn't null, otherwise the second."),
    scalar(
        "iif",
        &[sig("condition, value [, condition, value, ...] [, else]", "any")],
        "Value paired with the first true condition, otherwise the else value or null.",
    )
    .since("SQLite 3.32"),
    scalar(
        "instr",
        &[sig("text, search", "integer")],
        "Position of the first occurrence of search in text, from 1, or 0 if absent.",
    ),
    scalar("json", &[sig("json", "text")], "Minified JSON text, after checking that the argument is valid JSON."),
    scalar("json_array", &[sig("[value, ...]", "text")], "JSON array of the arguments."),
    scalar(
        "json_array_insert",
        &[sig("json, path, value [, path, value, ...]", "text")],
        "JSON with values inserted into arrays at the given element paths.",
    )
    .since("SQLite 3.53"),
    scalar(
        "json_array_length",
        &[sig("json [, path]", "integer")],
        "Number of elements in a JSON array, or 0 for other JSON values.",
    ),
    table("json_each", &[sig("json [, path]", "table")], "One row per element of a JSON array or object."),
    scalar(
        "json_error_position",
        &[sig("json", "integer")],
        "Character position of the first syntax error in JSON, or 0 if it is valid.",
    )
    .since("SQLite 3.42"),
    scalar(
        "json_extract",
        &[sig("json, path [, ...]", "any")],
        "Value at a path in JSON, or a JSON array of values for several paths.",
    ),
    aggregate("json_group_array", &[sig("value", "text")], "JSON array of the values in the group."),
    aggregate("json_group_object", &[sig("name, value", "text")], "JSON object of the name/value pairs in the group."),
    scalar(
        "json_insert",
        &[sig("json, path, value [, path, value, ...]", "text")],
        "JSON with values added at paths that don't exist yet.",
    ),
    scalar(
        "json_object",
        &[sig("[label, value, ...]", "text")],
        "JSON object built from label and value argument pairs.",
    ),
    scalar("json_patch", &[sig("target, patch", "text")], "Target JSON with an RFC 7396 merge patch applied."),
    scalar(
        "json_pretty",
        &[sig("json [, indent]", "text")],
        "JSON indented for reading, by four spaces or the given text per level.",
    )
    .since("SQLite 3.46"),
    scalar("json_quote", &[sig("value", "text")], "SQL value converted to its JSON representation."),
    scalar("json_remove", &[sig("json, path [, ...]", "text")], "JSON with the elements at the given paths removed."),
    scalar(
        "json_replace",
        &[sig("json, path, value [, path, value, ...]", "text")],
        "JSON with values overwritten at paths that already exist.",
    ),
    scalar(
        "json_set",
        &[sig("json, path, value [, path, value, ...]", "text")],
        "JSON with values written at the given paths, created or overwritten.",
    ),
    table(
        "json_tree",
        &[sig("json [, path]", "table")],
        "One row per element of a JSON value, walking nested arrays and objects.",
    ),
    scalar(
        "json_type",
        &[sig("json [, path]", "text")],
        "Type of a JSON value: null, true, false, integer, real, text, array or object.",
    ),
    scalar(
        "json_valid",
        &[sig("json [, flags]", "integer")],
        "Whether the argument is well-formed JSON, or JSONB per the flags, as 1 or 0.",
    ),
    scalar("jsonb", &[sig("json", "blob")], "Binary JSONB form of a JSON value.").since("SQLite 3.45"),
    scalar("jsonb_array", &[sig("[value, ...]", "blob")], "JSONB array of the arguments.").since("SQLite 3.45"),
    scalar(
        "jsonb_array_insert",
        &[sig("json, path, value [, path, value, ...]", "blob")],
        "JSONB with values inserted into arrays at the given element paths.",
    )
    .since("SQLite 3.53"),
    table(
        "jsonb_each",
        &[sig("json [, path]", "table")],
        "Like json_each, but arrays and objects in the value column are JSONB.",
    )
    .since("SQLite 3.51"),
    scalar(
        "jsonb_extract",
        &[sig("json, path [, ...]", "any")],
        "Value at a path in JSON, with arrays and objects returned as JSONB.",
    )
    .since("SQLite 3.45"),
    aggregate("jsonb_group_array", &[sig("value", "blob")], "JSONB array of the values in the group.")
        .since("SQLite 3.45"),
    aggregate(
        "jsonb_group_object",
        &[sig("name, value", "blob")],
        "JSONB object of the name/value pairs in the group.",
    )
    .since("SQLite 3.45"),
    scalar(
        "jsonb_insert",
        &[sig("json, path, value [, path, value, ...]", "blob")],
        "JSONB with values added at paths that don't exist yet.",
    )
    .since("SQLite 3.45"),
    scalar(
        "jsonb_object",
        &[sig("[label, value, ...]", "blob")],
        "JSONB object built from label and value argument pairs.",
    )
    .since("SQLite 3.45"),
    scalar("jsonb_patch", &[sig("target, patch", "blob")], "Target as JSONB with an RFC 7396 merge patch applied.")
        .since("SQLite 3.45"),
    scalar("jsonb_remove", &[sig("json, path [, ...]", "blob")], "JSONB with the elements at the given paths removed.")
        .since("SQLite 3.45"),
    scalar(
        "jsonb_replace",
        &[sig("json, path, value [, path, value, ...]", "blob")],
        "JSONB with values overwritten at paths that already exist.",
    )
    .since("SQLite 3.45"),
    scalar(
        "jsonb_set",
        &[sig("json, path, value [, path, value, ...]", "blob")],
        "JSONB with values written at the given paths, created or overwritten.",
    )
    .since("SQLite 3.45"),
    table(
        "jsonb_tree",
        &[sig("json [, path]", "table")],
        "Like json_tree, but arrays and objects in the value column are JSONB.",
    )
    .since("SQLite 3.51"),
    scalar(
        "julianday",
        &[sig("time-value [, modifier, ...]", "real")],
        "Julian day number as a real, after applying the modifiers.",
    ),
    window(
        "lag",
        &[sig("value [, offset [, default]]", "any")],
        "Value from the row offset rows before the current one, or the default.",
    ),
    scalar(
        "last_insert_rowid",
        &[sig("", "integer")],
        "Rowid of the most recent successful INSERT on this connection.",
    ),
    window(
        "lead",
        &[sig("value [, offset [, default]]", "any")],
        "Value from the row offset rows after the current one, or the default.",
    ),
    scalar("length", &[sig("value", "integer")], "Characters in a text value, or bytes in a blob."),
    scalar(
        "like",
        &[sig("pattern, text [, escape]", "integer")],
        "Whether the text matches a LIKE pattern, as 1 or 0.",
    ),
    scalar(
        "likelihood",
        &[sig("value, probability", "any")],
        "Returns the value unchanged; a planner hint that it is true with that probability.",
    ),
    scalar("likely", &[sig("value", "any")], "Returns the value unchanged; a planner hint that it is usually true."),
    scalar("ln", &[sig("x", "real")], "Natural logarithm.").since("SQLite 3.35"),
    scalar(
        "log",
        &[sig("x", "real"), sig("base, x", "real")],
        "Base-10 logarithm, or the logarithm in the given base.",
    )
    .since("SQLite 3.35"),
    scalar("log10", &[sig("x", "real")], "Base-10 logarithm.").since("SQLite 3.35"),
    scalar("log2", &[sig("x", "real")], "Base-2 logarithm.").since("SQLite 3.35"),
    scalar(
        "ltrim",
        &[sig("text [, characters]", "text")],
        "Removes the given characters, or spaces, from the start of the text.",
    ),
    scalar("mod", &[sig("x, y", "real")], "Remainder of x divided by y; unlike %, it works for non-integers.")
        .since("SQLite 3.35"),
    window(
        "nth_value",
        &[sig("value, n", "any")],
        "Value from the n-th row of the window frame, or null if there is none.",
    ),
    window("ntile", &[sig("n", "integer")], "Bucket from 1 to n, splitting the partition into n near-equal groups."),
    scalar("octet_length", &[sig("value", "integer")], "Bytes in the encoding of a text value.").since("SQLite 3.43"),
    window("percent_rank", &[sig("", "real")], "Relative rank of the current row, from 0 to 1."),
    scalar("pi", &[sig("", "real")], "The value of pi.").since("SQLite 3.35"),
    scalar("pow", &[sig("x, y", "real")], "Value of x raised to the power y.").since("SQLite 3.35"),
    scalar("power", &[sig("x, y", "real")], "Value of x raised to the power y; alias of pow.").since("SQLite 3.35"),
    table("pragma_compile_options", &[sig("", "table")], "Compile-time options SQLite was built with, one per row."),
    table(
        "pragma_database_list",
        &[sig("", "table")],
        "Databases attached to the connection, with their names and files.",
    ),
    table(
        "pragma_foreign_key_check",
        &[sig("[table [, schema]]", "table")],
        "Rows that violate a foreign key, in one table or all of them.",
    ),
    table(
        "pragma_foreign_key_list",
        &[sig("table [, schema]", "table")],
        "Foreign keys of a table, one row per referencing column.",
    ),
    table(
        "pragma_function_list",
        &[sig("", "table")],
        "Functions known to the connection, with their kind, arity and flags.",
    ),
    table("pragma_index_info", &[sig("index [, schema]", "table")], "Key columns of an index, in index order."),
    table(
        "pragma_index_list",
        &[sig("table [, schema]", "table")],
        "Indexes of a table, with uniqueness, origin and whether each is partial.",
    ),
    table(
        "pragma_index_xinfo",
        &[sig("index [, schema]", "table")],
        "Every column of an index, with sort order, collation and whether it is a key.",
    ),
    table(
        "pragma_table_info",
        &[sig("table [, schema]", "table")],
        "Columns of a table or view: name, type, not-null, default and key position.",
    ),
    table(
        "pragma_table_list",
        &[sig("[table]", "table")],
        "Tables and views in the attached databases, with type and column count.",
    )
    .since("SQLite 3.37"),
    table(
        "pragma_table_xinfo",
        &[sig("table [, schema]", "table")],
        "Like pragma_table_info, plus hidden and generated columns.",
    ),
    scalar(
        "printf",
        &[sig("format [, value, ...]", "text")],
        "Text built from a printf-style format string; the original name of format.",
    ),
    scalar("quote", &[sig("value", "text")], "SQL literal for a value, ready to use in a statement."),
    scalar("radians", &[sig("x", "real")], "Converts degrees to radians.").since("SQLite 3.35"),
    scalar("random", &[sig("", "integer")], "Pseudo-random 64-bit signed integer."),
    scalar("randomblob", &[sig("n", "blob")], "Blob of n pseudo-random bytes."),
    scalar(
        "rtrim",
        &[sig("text [, characters]", "text")],
        "Removes the given characters, or spaces, from the end of the text.",
    ),
    scalar("sign", &[sig("x", "integer")], "Sign of a number as -1, 0 or 1, or null if the argument isn't numeric.")
        .since("SQLite 3.35"),
    scalar("sin", &[sig("x", "real")], "Sine of an angle in radians.").since("SQLite 3.35"),
    scalar("sinh", &[sig("x", "real")], "Hyperbolic sine.").since("SQLite 3.35"),
    scalar(
        "snippet",
        &[sig("fts_table, column_index, open, close, ellipsis, max_tokens", "text")],
        "Short fragment of an FTS5 column around the matches, with markup.",
    ),
    scalar(
        "sqlite_compileoption_get",
        &[sig("n", "text")],
        "The n-th compile-time option SQLite was built with, or null past the end.",
    ),
    scalar(
        "sqlite_compileoption_used",
        &[sig("option", "integer")],
        "Whether SQLite was built with the named compile-time option, as 1 or 0.",
    ),
    scalar("sqlite_source_id", &[sig("", "text")], "Date, time and check-in hash of the SQLite source code in use."),
    scalar("sqlite_version", &[sig("", "text")], "Version string of the running SQLite library."),
    scalar("sqrt", &[sig("x", "real")], "Square root.").since("SQLite 3.35"),
    scalar(
        "strftime",
        &[sig("format, time-value [, modifier, ...]", "text")],
        "Date and time formatted with % substitutions, after applying the modifiers.",
    ),
    aggregate(
        "string_agg",
        &[sig("value, separator", "text")],
        "Non-null values of the group joined as text with the separator.",
    )
    .since("SQLite 3.44"),
    scalar(
        "substr",
        &[sig("text, start [, length]", "text")],
        "Part of a text, or bytes of a blob, from a start position counting from 1.",
    ),
    scalar(
        "substring",
        &[sig("text, start [, length]", "text")],
        "Alias of substr: part of a text or blob from a start position.",
    )
    .since("SQLite 3.34"),
    scalar("tan", &[sig("x", "real")], "Tangent of an angle in radians.").since("SQLite 3.35"),
    scalar("tanh", &[sig("x", "real")], "Hyperbolic tangent.").since("SQLite 3.35"),
    scalar("time", &[sig("time-value [, modifier, ...]", "text")], "Time as HH:MM:SS, after applying the modifiers."),
    scalar("timediff", &[sig("a, b", "text")], "Time to add to b to reach a, as text like +YYYY-MM-DD HH:MM:SS.SSS.")
        .since("SQLite 3.43"),
    aggregate("total", &[sig("value", "real")], "Sum of the non-null values as a real, or 0.0 when there are none."),
    scalar(
        "total_changes",
        &[sig("", "integer")],
        "Rows changed by INSERT, UPDATE and DELETE since the connection opened.",
    ),
    scalar(
        "trim",
        &[sig("text [, characters]", "text")],
        "Removes the given characters, or spaces, from both ends of the text.",
    ),
    scalar("trunc", &[sig("x", "numeric")], "Integer part of a number, rounding toward zero.").since("SQLite 3.35"),
    scalar("typeof", &[sig("value", "text")], "Storage class of a value: null, integer, real, text or blob."),
    scalar(
        "unhex",
        &[sig("hex [, separators]", "blob")],
        "Blob decoded from hexadecimal text, allowing the separators between pairs.",
    )
    .since("SQLite 3.41"),
    scalar("unicode", &[sig("text", "integer")], "Unicode code point of the first character of the text."),
    scalar(
        "unistr",
        &[sig("text", "text")],
        "Text with backslash escapes like \\u00e9 decoded into Unicode characters.",
    )
    .since("SQLite 3.50"),
    scalar(
        "unistr_quote",
        &[sig("value", "text")],
        "Like quote, but writes control characters in text as unistr() escapes.",
    )
    .since("SQLite 3.50"),
    scalar(
        "unixepoch",
        &[sig("time-value [, modifier, ...]", "integer")],
        "Seconds since 1970-01-01 UTC, after applying the modifiers.",
    )
    .since("SQLite 3.38"),
    scalar("unlikely", &[sig("value", "any")], "Returns the value unchanged; a planner hint that it is usually false."),
    scalar("zeroblob", &[sig("n", "blob")], "Blob of n zero bytes."),
];
