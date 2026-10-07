// MySQL and MariaDB built-ins. Overrides the shared core where MySQL differs.

use super::flags::{DEPRECATED, MARIADB_ONLY, MYSQL_ONLY, NO_PARENS, SYNTAX};
use super::{BuiltinFn, aggregate, scalar, sig, table, window};

pub(super) const PACK: &[BuiltinFn] = &[
    scalar("ABS", &[sig("X", "numeric")], "Absolute value of a number."),
    scalar("ACOS", &[sig("X", "DOUBLE")], "Arc cosine of a number, in radians."),
    scalar("ADD_MONTHS", &[sig("date, months", "DATE | DATETIME")], "Adds a number of months to a date.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 10.6.1"),
    scalar(
        "ADDDATE",
        &[sig("date, INTERVAL expr unit", "DATE | DATETIME"), sig("expr, days", "DATE | DATETIME")],
        "Adds an interval or a number of days to a date.",
    ),
    scalar("ADDTIME", &[sig("expr1, expr2", "TIME | DATETIME")], "Adds a time value to a time or datetime."),
    scalar(
        "AES_DECRYPT",
        &[sig("crypt_str, key_str [, init_vector]", "VARBINARY")],
        "Decrypts data encrypted with AES_ENCRYPT.",
    ),
    scalar("AES_ENCRYPT", &[sig("str, key_str [, init_vector]", "VARBINARY")], "Encrypts a string with AES."),
    aggregate("ANY_VALUE", &[sig("arg", "any")], "Any value from the group, exempt from ONLY_FULL_GROUP_BY checks.")
        .flags(MYSQL_ONLY)
        .since("MySQL 5.7"),
    scalar("ASCII", &[sig("str", "INT")], "Numeric code of the leftmost character of a string."),
    scalar("ASIN", &[sig("X", "DOUBLE")], "Arc sine of a number, in radians."),
    scalar(
        "ATAN",
        &[sig("X", "DOUBLE"), sig("Y, X", "DOUBLE")],
        "Arc tangent of X, or of Y/X using both signs to pick the quadrant.",
    ),
    scalar("ATAN2", &[sig("Y, X", "DOUBLE")], "Arc tangent of Y/X, using both signs to pick the quadrant."),
    aggregate("AVG", &[sig("[DISTINCT] expr", "DECIMAL | DOUBLE")], "Average of the non-NULL values."),
    scalar(
        "BENCHMARK",
        &[sig("count, expr", "INT")],
        "Evaluates an expression count times for timing, then returns 0.",
    ),
    scalar("BIN", &[sig("N", "VARCHAR")], "Binary representation of a number, as a string."),
    scalar("BIN_TO_UUID", &[sig("binary_uuid [, swap_flag]", "VARCHAR")], "Converts a binary UUID to its text form.")
        .flags(MYSQL_ONLY)
        .since("MySQL 8.0"),
    aggregate("BIT_AND", &[sig("expr", "BIGINT UNSIGNED")], "Bitwise AND of all values in a group."),
    scalar("BIT_COUNT", &[sig("N", "INT")], "Number of bits that are set in a number."),
    scalar("BIT_LENGTH", &[sig("str", "INT")], "Length of a string in bits."),
    aggregate("BIT_OR", &[sig("expr", "BIGINT UNSIGNED")], "Bitwise OR of all values in a group."),
    aggregate("BIT_XOR", &[sig("expr", "BIGINT UNSIGNED")], "Bitwise XOR of all values in a group."),
    scalar(
        "CAST",
        &[sig("expr AS type", "type")],
        "Converts a value to a type such as CHAR, SIGNED, DECIMAL or DATETIME.",
    )
    .flags(SYNTAX),
    scalar("CEIL", &[sig("X", "numeric")], "Smallest integer not less than the argument, like CEILING."),
    scalar("CEILING", &[sig("X", "numeric")], "Smallest integer not less than the argument."),
    scalar(
        "CHAR",
        &[sig("N [, N] ... [USING charset_name]", "VARBINARY")],
        "String made of the characters with the given integer codes.",
    ),
    scalar("CHAR_LENGTH", &[sig("str", "INT")], "Length of a string in characters."),
    scalar("CHARACTER_LENGTH", &[sig("str", "INT")], "Length of a string in characters, like CHAR_LENGTH."),
    scalar("CHARSET", &[sig("str", "VARCHAR")], "Character set of a string."),
    scalar("CHR", &[sig("N", "VARCHAR")], "Character with the given code, as a one-character string.")
        .flags(MARIADB_ONLY),
    scalar(
        "COALESCE",
        &[sig("value [, value] ...", "any")],
        "First argument that isn't NULL, or NULL if they all are.",
    ),
    scalar("COERCIBILITY", &[sig("str", "INT")], "Collation coercibility of a string, lower values taking precedence."),
    scalar("COLLATION", &[sig("str", "VARCHAR")], "Collation of a string."),
    scalar("COMPRESS", &[sig("string_to_compress", "BLOB")], "Compresses a string with zlib."),
    scalar(
        "CONCAT",
        &[sig("str [, str] ...", "VARCHAR")],
        "Joins the arguments into one string, or NULL if any argument is NULL.",
    ),
    scalar(
        "CONCAT_WS",
        &[sig("separator, str [, str] ...", "VARCHAR")],
        "Joins the arguments with a separator, skipping NULL values.",
    ),
    scalar("CONNECTION_ID", &[sig("", "BIGINT")], "ID of the current connection."),
    scalar("CONV", &[sig("N, from_base, to_base", "VARCHAR")], "Converts a number between bases, as a string."),
    scalar(
        "CONVERT",
        &[sig("expr, type", "type"), sig("expr USING charset_name", "VARCHAR")],
        "Converts a value to a type, or a string to another character set.",
    )
    .flags(SYNTAX),
    scalar(
        "CONVERT_TZ",
        &[sig("dt, from_tz, to_tz", "DATETIME")],
        "Converts a datetime from one time zone to another.",
    ),
    scalar("COS", &[sig("X", "DOUBLE")], "Cosine of an angle in radians."),
    scalar("COT", &[sig("X", "DOUBLE")], "Cotangent of an angle in radians."),
    aggregate(
        "COUNT",
        &[sig("*", "BIGINT"), sig("expr", "BIGINT"), sig("DISTINCT expr [, expr] ...", "BIGINT")],
        "Number of rows, of non-NULL values, or of distinct values.",
    ),
    scalar("CRC32", &[sig("expr", "INT UNSIGNED")], "32-bit cyclic redundancy check value of a string."),
    scalar(
        "CRC32C",
        &[sig("[par,] expr", "INT UNSIGNED")],
        "CRC-32C checksum of a string, optionally continuing from par.",
    )
    .flags(MARIADB_ONLY)
    .since("MariaDB 10.8"),
    window("CUME_DIST", &[sig("", "DOUBLE")], "Cumulative distribution of the current row within its partition.")
        .since("MySQL 8.0"),
    scalar("CURDATE", &[sig("", "DATE")], "Current date."),
    scalar("CURRENT_DATE", &[sig("", "DATE")], "Current date, like CURDATE.").flags(NO_PARENS),
    scalar("CURRENT_ROLE", &[sig("", "VARCHAR")], "Roles active in the current session.").since("MySQL 8.0"),
    scalar("CURRENT_TIME", &[sig("", "TIME")], "Current time, like CURTIME.").flags(NO_PARENS),
    scalar("CURRENT_TIMESTAMP", &[sig("", "DATETIME")], "Current date and time, like NOW.").flags(NO_PARENS),
    scalar("CURRENT_USER", &[sig("", "VARCHAR")], "Account the server used to authenticate the client, as user@host.")
        .flags(NO_PARENS),
    scalar("CURTIME", &[sig("[fsp]", "TIME")], "Current time, optionally with fractional seconds."),
    scalar("DATABASE", &[sig("", "VARCHAR")], "Name of the default database, or NULL if there is none."),
    scalar("DATE", &[sig("expr", "DATE")], "Date part of a date or datetime expression."),
    scalar(
        "DATE_ADD",
        &[sig("date, INTERVAL expr unit", "DATE | DATETIME")],
        "Adds a time interval to a date or datetime.",
    ),
    scalar(
        "DATE_FORMAT",
        &[sig("date, format", "VARCHAR")],
        "Formats a date or datetime with % specifiers such as %Y-%m-%d.",
    ),
    scalar(
        "DATE_SUB",
        &[sig("date, INTERVAL expr unit", "DATE | DATETIME")],
        "Subtracts a time interval from a date or datetime.",
    ),
    scalar("DATEDIFF", &[sig("expr1, expr2", "INT")], "Number of days from the second date to the first."),
    scalar("DAY", &[sig("date", "INT")], "Day of the month, like DAYOFMONTH."),
    scalar("DAYNAME", &[sig("date", "VARCHAR")], "Name of the weekday of a date."),
    scalar("DAYOFMONTH", &[sig("date", "INT")], "Day of the month, from 1 to 31."),
    scalar("DAYOFWEEK", &[sig("date", "INT")], "Weekday index of a date, from 1 for Sunday to 7 for Saturday."),
    scalar("DAYOFYEAR", &[sig("date", "INT")], "Day of the year, from 1 to 366."),
    scalar("DEFAULT", &[sig("col_name", "any")], "Default value of a table column."),
    scalar("DEGREES", &[sig("X", "DOUBLE")], "Converts radians to degrees."),
    window("DENSE_RANK", &[sig("", "BIGINT")], "Rank of the current row within its partition, without gaps.")
        .since("MySQL 8.0"),
    scalar("ELT", &[sig("N, str1 [, str2] ...", "VARCHAR")], "String at position N of the remaining arguments."),
    scalar("EXP", &[sig("X", "DOUBLE")], "Value of e raised to the power of X."),
    scalar(
        "EXPORT_SET",
        &[sig("bits, on, off [, separator [, number_of_bits]]", "VARCHAR")],
        "Lists the on or off string for each bit of a number.",
    ),
    scalar("EXTRACT", &[sig("unit FROM date", "INT")], "Part of a date or time, such as YEAR or HOUR.").flags(SYNTAX),
    scalar(
        "ExtractValue",
        &[sig("xml_frag, xpath_expr", "VARCHAR")],
        "Text of the XML nodes that an XPath expression selects.",
    ),
    scalar(
        "FIELD",
        &[sig("str, str1 [, str2] ...", "INT")],
        "Position of the first argument among the others, or 0 if absent.",
    ),
    scalar(
        "FIND_IN_SET",
        &[sig("str, strlist", "INT")],
        "Position of a string in a comma-separated list, or 0 if absent.",
    ),
    window("FIRST_VALUE", &[sig("expr", "any")], "Value from the first row of the window frame.").since("MySQL 8.0"),
    scalar("FLOOR", &[sig("X", "numeric")], "Largest integer not greater than the argument."),
    scalar(
        "FORMAT",
        &[sig("X, D [, locale]", "VARCHAR")],
        "Formats a number with thousands separators and D decimal places.",
    ),
    scalar("FORMAT_BYTES", &[sig("count", "VARCHAR")], "Byte count in human-readable units such as KiB or MiB.")
        .since("MySQL 8.0.16"),
    scalar(
        "FORMAT_PICO_TIME",
        &[sig("time_val", "VARCHAR")],
        "Picosecond count in human-readable units such as ms or s.",
    )
    .since("MySQL 8.0.16"),
    scalar(
        "FOUND_ROWS",
        &[sig("", "BIGINT")],
        "Rows the last SELECT found, ignoring LIMIT if it used SQL_CALC_FOUND_ROWS.",
    ),
    scalar("FROM_BASE64", &[sig("str", "VARBINARY")], "Decodes a base-64 encoded string."),
    scalar("FROM_DAYS", &[sig("N", "DATE")], "Date for a day number counted from year 0."),
    scalar(
        "FROM_UNIXTIME",
        &[sig("unix_timestamp", "DATETIME"), sig("unix_timestamp, format", "VARCHAR")],
        "Datetime of a Unix timestamp, optionally formatted as a string.",
    ),
    scalar(
        "GET_FORMAT",
        &[sig("{DATE | TIME | DATETIME}, {'EUR' | 'USA' | 'JIS' | 'ISO' | 'INTERNAL'}", "VARCHAR")],
        "Format string in a standard style, for DATE_FORMAT and STR_TO_DATE.",
    ),
    scalar("GET_LOCK", &[sig("str, timeout", "INT")], "Acquires a named lock, waiting up to timeout seconds."),
    scalar(
        "GREATEST",
        &[sig("value1, value2 [, value] ...", "any")],
        "Largest argument, or NULL if any argument is NULL.",
    ),
    aggregate(
        "GROUP_CONCAT",
        &[sig("[DISTINCT] expr [, expr] ... [ORDER BY ...] [SEPARATOR str]", "TEXT")],
        "Joins the non-NULL values of a group into one string.",
    ),
    aggregate(
        "GROUPING",
        &[sig("expr [, expr] ...", "INT")],
        "Flags the WITH ROLLUP super-aggregate rows for GROUP BY expressions.",
    )
    .flags(MYSQL_ONLY)
    .since("MySQL 8.0"),
    scalar("HEX", &[sig("str", "VARCHAR"), sig("N", "VARCHAR")], "Hexadecimal representation of a string or number."),
    scalar("HOUR", &[sig("time", "INT")], "Hour part of a time or datetime."),
    scalar("ICU_VERSION", &[sig("", "VARCHAR")], "Version of the ICU library behind the regular expression functions.")
        .flags(MYSQL_ONLY)
        .since("MySQL 8.0.4"),
    scalar("IF", &[sig("expr1, expr2, expr3", "any")], "Second argument if the first is true, otherwise the third."),
    scalar("IFNULL", &[sig("expr1, expr2", "any")], "First argument unless it is NULL, otherwise the second."),
    scalar("INET6_ATON", &[sig("expr", "VARBINARY")], "Binary form of an IPv4 or IPv6 address."),
    scalar("INET6_NTOA", &[sig("expr", "VARCHAR")], "Text form of a binary IPv4 or IPv6 address."),
    scalar("INET_ATON", &[sig("expr", "BIGINT")], "Integer value of a dotted-quad IPv4 address."),
    scalar("INET_NTOA", &[sig("expr", "VARCHAR")], "Dotted-quad IPv4 address of an integer."),
    scalar(
        "INSERT",
        &[sig("str, pos, len, newstr", "VARCHAR")],
        "Replaces len characters of a string at a position with another string.",
    ),
    scalar("INSTR", &[sig("str, substr", "INT")], "Position of the first occurrence of a substring, or 0 if absent."),
    scalar("IS_FREE_LOCK", &[sig("str", "INT")], "Whether a named lock is free, as 1 or 0."),
    scalar("IS_IPV4", &[sig("expr", "INT")], "Whether a string is a valid IPv4 address, as 1 or 0."),
    scalar("IS_IPV4_COMPAT", &[sig("expr", "INT")], "Whether a binary IPv6 address is IPv4-compatible."),
    scalar("IS_IPV4_MAPPED", &[sig("expr", "INT")], "Whether a binary IPv6 address is IPv4-mapped."),
    scalar("IS_IPV6", &[sig("expr", "INT")], "Whether a string is a valid IPv6 address, as 1 or 0."),
    scalar("IS_USED_LOCK", &[sig("str", "BIGINT")], "Connection ID holding a named lock, or NULL if it is free."),
    scalar("IS_UUID", &[sig("string_uuid", "INT")], "Whether a string is a valid UUID, as 1 or 0.")
        .flags(MYSQL_ONLY)
        .since("MySQL 8.0"),
    scalar("ISNULL", &[sig("expr", "INT")], "Whether an expression is NULL, as 1 or 0."),
    scalar("JSON_ARRAY", &[sig("[val [, val] ...]", "JSON")], "JSON array of the arguments."),
    scalar(
        "JSON_ARRAY_APPEND",
        &[sig("json_doc, path, val [, path, val] ...", "JSON")],
        "Appends values to the arrays at the given paths.",
    ),
    scalar(
        "JSON_ARRAY_INSERT",
        &[sig("json_doc, path, val [, path, val] ...", "JSON")],
        "Inserts values into arrays at the given array positions.",
    ),
    scalar("JSON_ARRAY_INTERSECT", &[sig("arr1, arr2", "JSON")], "JSON array of the elements two arrays share.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 11.2"),
    aggregate("JSON_ARRAYAGG", &[sig("col_or_expr", "JSON")], "JSON array of the values of a group.")
        .since("MySQL 5.7.22"),
    scalar("JSON_COMPACT", &[sig("json_doc", "JSON")], "JSON document with all unneeded spaces removed.")
        .flags(MARIADB_ONLY),
    scalar(
        "JSON_CONTAINS",
        &[sig("target, candidate [, path]", "INT")],
        "Whether a JSON document contains another one, optionally at a path.",
    ),
    scalar(
        "JSON_CONTAINS_PATH",
        &[sig("json_doc, one_or_all, path [, path] ...", "INT")],
        "Whether a JSON document has data at one or all of the given paths.",
    ),
    scalar("JSON_DEPTH", &[sig("json_doc", "INT")], "Maximum nesting depth of a JSON document."),
    scalar("JSON_DETAILED", &[sig("json_doc [, tab_size]", "JSON")], "JSON document indented for reading.")
        .flags(MARIADB_ONLY),
    scalar(
        "JSON_EQUALS",
        &[sig("json1, json2", "INT")],
        "Whether two JSON documents are equal, ignoring key order and spacing.",
    )
    .flags(MARIADB_ONLY)
    .since("MariaDB 10.7"),
    scalar("JSON_EXISTS", &[sig("json_doc, path", "INT")], "Whether a JSON document has data at a path, as 1 or 0.")
        .flags(MARIADB_ONLY),
    scalar(
        "JSON_EXTRACT",
        &[sig("json_doc, path [, path] ...", "JSON")],
        "Value at the given paths of a JSON document.",
    ),
    scalar(
        "JSON_INSERT",
        &[sig("json_doc, path, val [, path, val] ...", "JSON")],
        "Adds values at paths that don't exist yet, keeping existing ones.",
    ),
    scalar("JSON_KEY_VALUE", &[sig("obj, path", "JSON")], "Key-value pairs of the object at a path, as a JSON array.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 11.2"),
    scalar("JSON_KEYS", &[sig("json_doc [, path]", "JSON")], "Keys of a JSON object, as a JSON array."),
    scalar("JSON_LENGTH", &[sig("json_doc [, path]", "INT")], "Number of elements in a JSON document or at a path."),
    scalar("JSON_LOOSE", &[sig("json_doc", "JSON")], "JSON document with spaces added for readability.")
        .flags(MARIADB_ONLY),
    scalar(
        "JSON_MERGE",
        &[sig("json_doc, json_doc [, json_doc] ...", "JSON")],
        "Deprecated synonym for JSON_MERGE_PRESERVE.",
    )
    .flags(DEPRECATED),
    scalar(
        "JSON_MERGE_PATCH",
        &[sig("json_doc, json_doc [, json_doc] ...", "JSON")],
        "Merges JSON documents as RFC 7396 patches, so later keys win.",
    )
    .since("MySQL 5.7.22"),
    scalar(
        "JSON_MERGE_PRESERVE",
        &[sig("json_doc, json_doc [, json_doc] ...", "JSON")],
        "Merges JSON documents, keeping every value of duplicate keys in arrays.",
    )
    .since("MySQL 5.7.22"),
    scalar("JSON_NORMALIZE", &[sig("json", "JSON")], "JSON document with sorted keys and no spaces, for comparisons.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 10.7"),
    scalar("JSON_OBJECT", &[sig("[key, val [, key, val] ...]", "JSON")], "JSON object of the given key-value pairs."),
    scalar(
        "JSON_OBJECT_FILTER_KEYS",
        &[sig("obj, array_keys", "JSON")],
        "JSON object with only the keys listed in a JSON array.",
    )
    .flags(MARIADB_ONLY)
    .since("MariaDB 11.2"),
    scalar("JSON_OBJECT_TO_ARRAY", &[sig("obj", "JSON")], "JSON object converted to an array of key-value pairs.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 11.2"),
    aggregate("JSON_OBJECTAGG", &[sig("key, value", "JSON")], "JSON object of the key-value pairs of a group.")
        .since("MySQL 5.7.22"),
    scalar(
        "JSON_OVERLAPS",
        &[sig("json_doc1, json_doc2", "INT")],
        "Whether two JSON documents share a key-value pair or array element.",
    )
    .since("MySQL 8.0.17"),
    scalar("JSON_PRETTY", &[sig("json_val", "TEXT")], "JSON document indented for reading.").since("MySQL 5.7.22"),
    scalar("JSON_QUERY", &[sig("json_doc, path", "JSON")], "Object or array at a path of a JSON document.")
        .flags(MARIADB_ONLY),
    scalar("JSON_QUOTE", &[sig("string", "VARCHAR")], "Quotes a string as a JSON string literal."),
    scalar(
        "JSON_REMOVE",
        &[sig("json_doc, path [, path] ...", "JSON")],
        "Removes the data at the given paths of a JSON document.",
    ),
    scalar(
        "JSON_REPLACE",
        &[sig("json_doc, path, val [, path, val] ...", "JSON")],
        "Replaces existing values at the given paths, adding nothing new.",
    ),
    scalar(
        "JSON_SCHEMA_VALID",
        &[sig("schema, document", "INT")],
        "Whether a JSON document validates against a JSON schema.",
    )
    .since("MySQL 8.0.17"),
    scalar(
        "JSON_SCHEMA_VALIDATION_REPORT",
        &[sig("schema, document", "JSON")],
        "Report on whether a JSON document validates against a JSON schema.",
    )
    .flags(MYSQL_ONLY)
    .since("MySQL 8.0.17"),
    scalar(
        "JSON_SEARCH",
        &[sig("json_doc, one_or_all, search_str [, escape_char [, path] ...]", "JSON")],
        "Paths to the strings in a JSON document that match a pattern.",
    ),
    scalar(
        "JSON_SET",
        &[sig("json_doc, path, val [, path, val] ...", "JSON")],
        "Inserts or replaces values at the given paths.",
    ),
    scalar("JSON_STORAGE_FREE", &[sig("json_val", "INT")], "Bytes freed in a JSON column value by partial updates.")
        .flags(MYSQL_ONLY)
        .since("MySQL 8.0"),
    scalar("JSON_STORAGE_SIZE", &[sig("json_val", "INT")], "Bytes used to store the binary form of a JSON document.")
        .flags(MYSQL_ONLY)
        .since("MySQL 5.7.22"),
    table(
        "JSON_TABLE",
        &[sig(
            "expr, path COLUMNS (name type PATH path | name FOR ORDINALITY | NESTED PATH path COLUMNS (...) [, ...])",
            "table",
        )],
        "Turns JSON data into relational rows and columns.",
    )
    .since("MySQL 8.0.4"),
    scalar("JSON_TYPE", &[sig("json_val", "VARCHAR")], "Type of a JSON value, such as OBJECT, ARRAY or INTEGER."),
    scalar("JSON_UNQUOTE", &[sig("json_val", "TEXT")], "Unquotes a JSON value and returns it as a string."),
    scalar("JSON_VALID", &[sig("val", "INT")], "Whether a value is valid JSON, as 1 or 0."),
    scalar(
        "JSON_VALUE",
        &[sig("json_doc, path [RETURNING type] [on_empty] [on_error]", "VARCHAR")],
        "Scalar at a path of a JSON document, optionally converted to a type.",
    )
    .since("MySQL 8.0.21"),
    scalar(
        "KDF",
        &[sig("key_str, salt [, {info | iterations} [, kdf_name [, width]]]", "VARBINARY")],
        "Derives a key from a password with a key derivation function.",
    )
    .flags(MARIADB_ONLY)
    .since("MariaDB 11.3"),
    window(
        "LAG",
        &[sig("expr [, N [, default]]", "any")],
        "Value from N rows before the current row in its partition.",
    )
    .since("MySQL 8.0"),
    scalar("LAST_DAY", &[sig("date", "DATE")], "Last day of the month of a date."),
    scalar(
        "LAST_INSERT_ID",
        &[sig("[expr]", "BIGINT")],
        "First AUTO_INCREMENT value generated by the session's last INSERT.",
    ),
    window("LAST_VALUE", &[sig("expr", "any")], "Value from the last row of the window frame.").since("MySQL 8.0"),
    scalar("LASTVAL", &[sig("sequence_name", "BIGINT")], "Last value the session got from a sequence.")
        .flags(MARIADB_ONLY),
    scalar("LCASE", &[sig("str", "VARCHAR")], "Converts a string to lowercase, like LOWER."),
    window(
        "LEAD",
        &[sig("expr [, N [, default]]", "any")],
        "Value from N rows after the current row in its partition.",
    )
    .since("MySQL 8.0"),
    scalar(
        "LEAST",
        &[sig("value1, value2 [, value] ...", "any")],
        "Smallest argument, or NULL if any argument is NULL.",
    ),
    scalar("LEFT", &[sig("str, len", "VARCHAR")], "Leftmost len characters of a string."),
    scalar("LENGTH", &[sig("str", "INT")], "Length of a string in bytes."),
    scalar("LENGTHB", &[sig("str", "INT")], "Length of a string in bytes, like LENGTH.").flags(MARIADB_ONLY),
    scalar("LN", &[sig("X", "DOUBLE")], "Natural logarithm of a number."),
    scalar("LOAD_FILE", &[sig("file_name", "LONGBLOB")], "Contents of a file on the server host."),
    scalar("LOCALTIME", &[sig("", "DATETIME")], "Current date and time, like NOW.").flags(NO_PARENS),
    scalar("LOCALTIMESTAMP", &[sig("", "DATETIME")], "Current date and time, like NOW.").flags(NO_PARENS),
    scalar(
        "LOCATE",
        &[sig("substr, str [, pos]", "INT")],
        "Position of the first occurrence of a substring, optionally from pos.",
    ),
    scalar("LOG", &[sig("X", "DOUBLE"), sig("B, X", "DOUBLE")], "Natural logarithm of X, or its logarithm to base B."),
    scalar("LOG10", &[sig("X", "DOUBLE")], "Base-10 logarithm of a number."),
    scalar("LOG2", &[sig("X", "DOUBLE")], "Base-2 logarithm of a number."),
    scalar("LOWER", &[sig("str", "VARCHAR")], "Converts a string to lowercase."),
    scalar("LPAD", &[sig("str, len, padstr", "VARCHAR")], "Left-pads a string with another string to len characters."),
    scalar("LTRIM", &[sig("str", "VARCHAR")], "Removes leading spaces."),
    scalar(
        "MAKE_SET",
        &[sig("bits, str1 [, str2] ...", "VARCHAR")],
        "Comma-separated list of the strings whose bits are set in a number.",
    ),
    scalar("MAKEDATE", &[sig("year, dayofyear", "DATE")], "Date from a year and a day of the year."),
    scalar("MAKETIME", &[sig("hour, minute, second", "TIME")], "Time from hour, minute and second values."),
    aggregate("MAX", &[sig("[DISTINCT] expr", "any")], "Largest non-NULL value."),
    scalar("MD5", &[sig("str", "VARCHAR")], "MD5 checksum of a string, as 32 hex digits."),
    window("MEDIAN", &[sig("expr", "DOUBLE")], "Median of the values in the window partition.").flags(MARIADB_ONLY),
    scalar("MICROSECOND", &[sig("expr", "INT")], "Microseconds part of a time or datetime."),
    scalar("MID", &[sig("str, pos [, len]", "VARCHAR")], "Part of a string from a position, like SUBSTRING."),
    aggregate("MIN", &[sig("[DISTINCT] expr", "any")], "Smallest non-NULL value."),
    scalar("MINUTE", &[sig("time", "INT")], "Minute part of a time or datetime."),
    scalar("MOD", &[sig("N, M", "numeric")], "Remainder of N divided by M."),
    scalar("MONTH", &[sig("date", "INT")], "Month of a date, from 1 to 12."),
    scalar("MONTHNAME", &[sig("date", "VARCHAR")], "Full name of the month of a date."),
    scalar(
        "NAME_CONST",
        &[sig("name, value", "any")],
        "Value that takes the given column name, as used in the binary log.",
    ),
    scalar("NATURAL_SORT_KEY", &[sig("str", "VARCHAR")], "Sort key that orders the numbers inside strings by value.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 10.7"),
    scalar("NEXTVAL", &[sig("sequence_name", "BIGINT")], "Next value of a sequence.").flags(MARIADB_ONLY),
    scalar("NOW", &[sig("[fsp]", "DATETIME")], "Date and time when the statement started."),
    window("NTH_VALUE", &[sig("expr, N", "any")], "Value from the N-th row of the window frame.").since("MySQL 8.0"),
    window("NTILE", &[sig("N", "BIGINT")], "Bucket from 1 to N that the current row falls into.").since("MySQL 8.0"),
    scalar("NULLIF", &[sig("expr1, expr2", "any")], "NULL if the arguments are equal, otherwise the first one."),
    scalar(
        "NVL",
        &[sig("expr1, expr2", "any")],
        "First argument unless it is NULL, otherwise the second, like IFNULL.",
    )
    .flags(MARIADB_ONLY),
    scalar(
        "NVL2",
        &[sig("expr1, expr2, expr3", "any")],
        "Second argument if the first isn't NULL, otherwise the third.",
    )
    .flags(MARIADB_ONLY),
    scalar("OCT", &[sig("N", "VARCHAR")], "Octal representation of a number, as a string."),
    scalar("OCTET_LENGTH", &[sig("str", "INT")], "Length of a string in bytes, like LENGTH."),
    scalar("ORD", &[sig("str", "INT")], "Code of the leftmost character of a string, including multibyte ones."),
    scalar("PASSWORD", &[sig("str", "VARCHAR")], "Password hash in the mysql_native_password format.")
        .flags(MARIADB_ONLY),
    window("PERCENT_RANK", &[sig("", "DOUBLE")], "Relative rank of the current row, from 0 to 1.").since("MySQL 8.0"),
    window(
        "PERCENTILE_CONT",
        &[sig("fraction", "DOUBLE")],
        "Interpolated value at a fraction of the WITHIN GROUP ordering.",
    )
    .flags(MARIADB_ONLY),
    window(
        "PERCENTILE_DISC",
        &[sig("fraction", "any")],
        "First value at or past a fraction of the WITHIN GROUP ordering.",
    )
    .flags(MARIADB_ONLY),
    scalar("PERIOD_ADD", &[sig("P, N", "INT")], "Adds N months to a period in YYMM or YYYYMM format."),
    scalar("PERIOD_DIFF", &[sig("P1, P2", "INT")], "Number of months between two periods in YYMM or YYYYMM format."),
    scalar("PI", &[sig("", "DOUBLE")], "Value of pi."),
    scalar("Point", &[sig("x, y", "POINT")], "Point from X and Y coordinates."),
    scalar("POSITION", &[sig("substr IN str", "INT")], "Position of the first occurrence of a substring, like LOCATE."),
    scalar("POW", &[sig("X, Y", "DOUBLE")], "X raised to the power of Y."),
    scalar("POWER", &[sig("X, Y", "DOUBLE")], "X raised to the power of Y, like POW."),
    scalar(
        "PS_CURRENT_THREAD_ID",
        &[sig("", "BIGINT UNSIGNED")],
        "Performance Schema thread ID of the current connection.",
    )
    .flags(MYSQL_ONLY)
    .since("MySQL 8.0.16"),
    scalar("PS_THREAD_ID", &[sig("connection_id", "BIGINT UNSIGNED")], "Performance Schema thread ID of a connection.")
        .flags(MYSQL_ONLY)
        .since("MySQL 8.0.16"),
    scalar("QUARTER", &[sig("date", "INT")], "Quarter of the year of a date, from 1 to 4."),
    scalar("QUOTE", &[sig("str", "VARCHAR")], "Quotes and escapes a string for use as an SQL string literal."),
    scalar("RADIANS", &[sig("X", "DOUBLE")], "Converts degrees to radians."),
    scalar("RAND", &[sig("[N]", "DOUBLE")], "Random value from 0 up to 1, repeatable when seeded with N."),
    scalar("RANDOM_BYTES", &[sig("len", "VARBINARY")], "Binary string of len random bytes."),
    window("RANK", &[sig("", "BIGINT")], "Rank of the current row within its partition, with gaps.").since("MySQL 8.0"),
    scalar(
        "REGEXP_INSTR",
        &[sig("expr, pat [, pos [, occurrence [, return_option [, match_type]]]]", "INT")],
        "Position of the substring that matches a regular expression.",
    )
    .since("MySQL 8.0.4"),
    scalar(
        "REGEXP_LIKE",
        &[sig("expr, pat [, match_type]", "INT")],
        "Whether a string matches a regular expression, as 1 or 0.",
    )
    .flags(MYSQL_ONLY)
    .since("MySQL 8.0.4"),
    scalar(
        "REGEXP_REPLACE",
        &[sig("expr, pat, repl [, pos [, occurrence [, match_type]]]", "VARCHAR")],
        "Replaces the substrings that match a regular expression.",
    )
    .since("MySQL 8.0.4"),
    scalar(
        "REGEXP_SUBSTR",
        &[sig("expr, pat [, pos [, occurrence [, match_type]]]", "VARCHAR")],
        "Substring that matches a regular expression.",
    )
    .since("MySQL 8.0.4"),
    scalar("RELEASE_ALL_LOCKS", &[sig("", "INT")], "Releases the session's named locks and returns how many it held."),
    scalar("RELEASE_LOCK", &[sig("str", "INT")], "Releases a named lock, returning 1 if this session held it."),
    scalar("REPEAT", &[sig("str, count", "VARCHAR")], "Repeats a string count times."),
    scalar(
        "REPLACE",
        &[sig("str, from_str, to_str", "VARCHAR")],
        "Replaces every occurrence of one substring with another.",
    ),
    scalar("REVERSE", &[sig("str", "VARCHAR")], "String with its characters in reverse order."),
    scalar("RIGHT", &[sig("str, len", "VARCHAR")], "Rightmost len characters of a string."),
    scalar("ROLES_GRAPHML", &[sig("", "TEXT")], "GraphML document of the role grants between accounts.")
        .flags(MYSQL_ONLY)
        .since("MySQL 8.0"),
    scalar("ROUND", &[sig("X [, D]", "numeric")], "Rounds a number to D decimal places, 0 by default."),
    scalar("ROW_COUNT", &[sig("", "BIGINT")], "Rows changed, deleted or inserted by the previous statement."),
    window("ROW_NUMBER", &[sig("", "BIGINT")], "Number of the current row within its partition, from 1.")
        .since("MySQL 8.0"),
    scalar("ROWNUM", &[sig("", "BIGINT")], "Number of rows accepted so far in the current context.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 10.6.1"),
    scalar("RPAD", &[sig("str, len, padstr", "VARCHAR")], "Right-pads a string with another string to len characters."),
    scalar("RTRIM", &[sig("str", "VARCHAR")], "Removes trailing spaces."),
    scalar("SCHEMA", &[sig("", "VARCHAR")], "Name of the default database, like DATABASE."),
    scalar("SEC_TO_TIME", &[sig("seconds", "TIME")], "Converts a number of seconds to a time value."),
    scalar("SECOND", &[sig("time", "INT")], "Second part of a time or datetime."),
    scalar("SESSION_USER", &[sig("", "VARCHAR")], "User name and host the client connected as, like USER."),
    scalar(
        "SETVAL",
        &[sig("sequence_name, next_value [, is_used [, round]]", "BIGINT")],
        "Sets the next value of a sequence.",
    )
    .flags(MARIADB_ONLY),
    scalar(
        "SFORMAT",
        &[sig("format, value [, value] ...", "TEXT")],
        "Formats values into a string with {} placeholders.",
    )
    .flags(MARIADB_ONLY)
    .since("MariaDB 10.7"),
    scalar("SHA", &[sig("str", "VARCHAR")], "SHA-1 checksum of a string, like SHA1."),
    scalar("SHA1", &[sig("str", "VARCHAR")], "SHA-1 checksum of a string, as 40 hex digits."),
    scalar(
        "SHA2",
        &[sig("str, hash_length", "VARCHAR")],
        "SHA-2 checksum of a string, with hash_length 224, 256, 384 or 512.",
    ),
    scalar("SIGN", &[sig("X", "INT")], "Sign of a number as -1, 0 or 1."),
    scalar("SIN", &[sig("X", "DOUBLE")], "Sine of an angle in radians."),
    scalar("SLEEP", &[sig("duration", "INT")], "Pauses for a number of seconds, then returns 0."),
    scalar("SOUNDEX", &[sig("str", "VARCHAR")], "Soundex string, for comparing how words sound."),
    scalar("SPACE", &[sig("N", "VARCHAR")], "String of N spaces."),
    scalar("SQRT", &[sig("X", "DOUBLE")], "Square root of a non-negative number."),
    scalar("ST_AsGeoJSON", &[sig("g [, max_dec_digits [, options]]", "JSON")], "GeoJSON object for a geometry."),
    scalar("ST_AsText", &[sig("g", "TEXT")], "Well-Known Text representation of a geometry."),
    scalar("ST_Contains", &[sig("g1, g2", "INT")], "Whether g1 completely contains g2."),
    scalar("ST_Distance", &[sig("g1, g2", "DOUBLE")], "Distance between two geometries."),
    scalar(
        "ST_Distance_Sphere",
        &[sig("g1, g2 [, radius]", "DOUBLE")],
        "Distance between two points on a sphere, in meters by default.",
    ),
    scalar("ST_GeomFromText", &[sig("wkt [, srid]", "GEOMETRY")], "Geometry from its Well-Known Text representation."),
    scalar("ST_Intersects", &[sig("g1, g2", "INT")], "Whether two geometries intersect."),
    scalar("ST_SRID", &[sig("g", "INT")], "Spatial reference system ID of a geometry."),
    scalar("ST_Within", &[sig("g1, g2", "INT")], "Whether g1 lies within g2."),
    scalar("ST_X", &[sig("p", "DOUBLE")], "X coordinate of a point."),
    scalar("ST_Y", &[sig("p", "DOUBLE")], "Y coordinate of a point."),
    scalar("STATEMENT_DIGEST", &[sig("statement", "VARCHAR")], "Digest hash of an SQL statement.")
        .flags(MYSQL_ONLY)
        .since("MySQL 8.0.4"),
    scalar("STATEMENT_DIGEST_TEXT", &[sig("statement", "TEXT")], "Normalized digest text of an SQL statement.")
        .flags(MYSQL_ONLY)
        .since("MySQL 8.0.4"),
    aggregate("STD", &[sig("expr", "DOUBLE")], "Population standard deviation, like STDDEV_POP."),
    aggregate("STDDEV", &[sig("expr", "DOUBLE")], "Population standard deviation, like STDDEV_POP."),
    aggregate("STDDEV_POP", &[sig("expr", "DOUBLE")], "Population standard deviation of the values."),
    aggregate("STDDEV_SAMP", &[sig("expr", "DOUBLE")], "Sample standard deviation of the values."),
    scalar(
        "STR_TO_DATE",
        &[sig("str, format", "DATE | TIME | DATETIME")],
        "Parses a string into a date, time or datetime using a format.",
    ),
    scalar("STRCMP", &[sig("expr1, expr2", "INT")], "Compares two strings, returning -1, 0 or 1."),
    scalar(
        "SUBDATE",
        &[sig("date, INTERVAL expr unit", "DATE | DATETIME"), sig("expr, days", "DATE | DATETIME")],
        "Subtracts an interval or a number of days from a date.",
    ),
    scalar(
        "SUBSTR",
        &[sig("str, pos [, len]", "VARCHAR"), sig("str FROM pos [FOR len]", "VARCHAR")],
        "Part of a string from a position, like SUBSTRING.",
    ),
    scalar(
        "SUBSTRING",
        &[sig("str, pos [, len]", "VARCHAR"), sig("str FROM pos [FOR len]", "VARCHAR")],
        "Part of a string from a position, optionally of a given length.",
    ),
    scalar(
        "SUBSTRING_INDEX",
        &[sig("str, delim, count", "VARCHAR")],
        "Part of a string before the count-th delimiter, from the right if negative.",
    ),
    scalar("SUBTIME", &[sig("expr1, expr2", "TIME | DATETIME")], "Subtracts a time value from a time or datetime."),
    aggregate("SUM", &[sig("[DISTINCT] expr", "DECIMAL | DOUBLE")], "Sum of the non-NULL values."),
    scalar("SYS_GUID", &[sig("", "VARCHAR")], "Globally unique identifier as 32 hex digits, without hyphens.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 10.6.1"),
    scalar("SYSDATE", &[sig("[fsp]", "DATETIME")], "Date and time at the moment the function runs."),
    scalar("SYSTEM_USER", &[sig("", "VARCHAR")], "User name and host the client connected as, like USER."),
    scalar("TAN", &[sig("X", "DOUBLE")], "Tangent of an angle in radians."),
    scalar("TIME", &[sig("expr", "TIME")], "Time part of a time or datetime expression."),
    scalar("TIME_FORMAT", &[sig("time, format", "VARCHAR")], "Formats a time value with % specifiers."),
    scalar("TIME_TO_SEC", &[sig("time", "INT")], "Converts a time value to a number of seconds."),
    scalar("TIMEDIFF", &[sig("expr1, expr2", "TIME")], "Difference between two times or datetimes, as a time."),
    scalar(
        "TIMESTAMP",
        &[sig("expr [, time_expr]", "DATETIME")],
        "Datetime from a date or datetime, optionally plus a time.",
    ),
    scalar(
        "TIMESTAMPADD",
        &[sig("unit, interval, datetime_expr", "DATE | DATETIME")],
        "Adds a whole number of units to a date or datetime.",
    ),
    scalar(
        "TIMESTAMPDIFF",
        &[sig("unit, datetime_expr1, datetime_expr2", "BIGINT")],
        "Whole units from the first datetime to the second.",
    ),
    scalar("TO_BASE64", &[sig("str", "VARCHAR")], "Encodes a string as base-64."),
    scalar(
        "TO_CHAR",
        &[sig("expr [, fmt]", "VARCHAR")],
        "Formats a date or time value as a string with an Oracle-style format.",
    )
    .flags(MARIADB_ONLY)
    .since("MariaDB 10.6.1"),
    scalar("TO_DAYS", &[sig("date", "INT")], "Day number of a date, counted from year 0."),
    scalar(
        "TO_NUMBER",
        &[sig("expr", "DOUBLE"), sig("str, format", "DOUBLE")],
        "Converts a string to a number, optionally with an Oracle-style format.",
    )
    .flags(MARIADB_ONLY)
    .since("MariaDB 12.2"),
    scalar("TO_SECONDS", &[sig("expr", "BIGINT")], "Seconds since the start of year 0 for a date or datetime."),
    scalar(
        "TRIM",
        &[sig("[{BOTH | LEADING | TRAILING} [remstr] FROM] str", "VARCHAR"), sig("[remstr FROM] str", "VARCHAR")],
        "Removes leading and trailing spaces, or another prefix or suffix.",
    ),
    scalar("TRUNC", &[sig("date [, fmt]", "DATETIME")], "Truncates a datetime to its day, month or year.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 12.2"),
    scalar("TRUNCATE", &[sig("X, D", "numeric")], "Truncates a number to D decimal places."),
    scalar("UCASE", &[sig("str", "VARCHAR")], "Converts a string to uppercase, like UPPER."),
    scalar("UNCOMPRESS", &[sig("string_to_uncompress", "BLOB")], "Decompresses a string compressed with COMPRESS."),
    scalar(
        "UNCOMPRESSED_LENGTH",
        &[sig("compressed_string", "INT")],
        "Length a COMPRESS result had before compression.",
    ),
    scalar("UNHEX", &[sig("str", "VARBINARY")], "Converts pairs of hexadecimal digits to the bytes they encode."),
    scalar(
        "UNIX_TIMESTAMP",
        &[sig("[date]", "BIGINT")],
        "Seconds since 1970-01-01 00:00:00 UTC, for now or a given date.",
    ),
    scalar(
        "UpdateXML",
        &[sig("xml_target, xpath_expr, new_xml", "VARCHAR")],
        "Replaces the XML fragment that an XPath expression selects.",
    ),
    scalar("UPPER", &[sig("str", "VARCHAR")], "Converts a string to uppercase."),
    scalar("USER", &[sig("", "VARCHAR")], "User name and host the client connected as."),
    scalar("UTC_DATE", &[sig("", "DATE")], "Current UTC date.").flags(NO_PARENS),
    scalar("UTC_TIME", &[sig("", "TIME")], "Current UTC time.").flags(NO_PARENS),
    scalar("UTC_TIMESTAMP", &[sig("", "DATETIME")], "Current UTC date and time.").flags(NO_PARENS),
    scalar("UUID", &[sig("", "VARCHAR")], "Version 1 UUID as a 36-character string."),
    scalar("UUID_SHORT", &[sig("", "BIGINT UNSIGNED")], "Unique 64-bit integer identifier."),
    scalar(
        "UUID_TO_BIN",
        &[sig("string_uuid [, swap_flag]", "BINARY(16)")],
        "Converts a text UUID to 16 bytes, optionally reordered for indexing.",
    )
    .flags(MYSQL_ONLY)
    .since("MySQL 8.0"),
    scalar("UUID_v4", &[sig("", "UUID")], "Random version 4 UUID.").flags(MARIADB_ONLY).since("MariaDB 11.7"),
    scalar("UUID_v7", &[sig("", "UUID")], "Time-ordered version 7 UUID.").flags(MARIADB_ONLY).since("MariaDB 11.7"),
    scalar(
        "VALIDATE_PASSWORD_STRENGTH",
        &[sig("str", "INT")],
        "Password strength from 0 to 100, from the validate_password component.",
    )
    .flags(MYSQL_ONLY),
    scalar(
        "VALUES",
        &[sig("col_name", "any")],
        "Value a column would get from the INSERT, in ON DUPLICATE KEY UPDATE.",
    ),
    aggregate("VAR_POP", &[sig("expr", "DOUBLE")], "Population variance of the values."),
    aggregate("VAR_SAMP", &[sig("expr", "DOUBLE")], "Sample variance of the values."),
    aggregate("VARIANCE", &[sig("expr", "DOUBLE")], "Population variance, like VAR_POP."),
    scalar(
        "VEC_DISTANCE",
        &[sig("v, s", "DOUBLE")],
        "Distance between two vectors, by the metric of the vector index.",
    )
    .flags(MARIADB_ONLY)
    .since("MariaDB 11.7"),
    scalar("VEC_DISTANCE_COSINE", &[sig("v, s", "DOUBLE")], "Cosine distance between two vectors.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 11.7"),
    scalar("VEC_DISTANCE_EUCLIDEAN", &[sig("v, s", "DOUBLE")], "Euclidean distance between two vectors.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 11.7"),
    scalar("VEC_FromText", &[sig("s", "VECTOR")], "Vector from a JSON array of numbers.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 11.7"),
    scalar("VEC_ToText", &[sig("v", "TEXT")], "JSON array of the numbers in a vector.")
        .flags(MARIADB_ONLY)
        .since("MariaDB 11.7"),
    scalar("VERSION", &[sig("", "VARCHAR")], "Version string of the server."),
    scalar(
        "WEEK",
        &[sig("date [, mode]", "INT")],
        "Week number of a date, with mode setting the first weekday and range.",
    ),
    scalar("WEEKDAY", &[sig("date", "INT")], "Weekday index of a date, from 0 for Monday to 6 for Sunday."),
    scalar("WEEKOFYEAR", &[sig("date", "INT")], "Calendar week of a date, from 1 to 53."),
    scalar(
        "WEIGHT_STRING",
        &[sig("str [AS {CHAR | BINARY}(N)]", "VARBINARY")],
        "Collation weight string used to compare and sort a string.",
    ),
    scalar("YEAR", &[sig("date", "INT")], "Year of a date."),
    scalar("YEARWEEK", &[sig("date [, mode]", "INT")], "Year and week of a date, as a YYYYWW number."),
];
