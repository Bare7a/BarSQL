// SQL Server built-ins. Overrides the shared core where T-SQL differs.

use super::flags::{DEPRECATED, NO_PARENS, SYNTAX};
use super::{BuiltinFn, aggregate, scalar, sig, table, window};

pub(super) const PACK: &[BuiltinFn] = &[
    scalar("ABS", &[sig("numeric_expression", "same type as numeric_expression")], "Absolute value of a number."),
    scalar("ACOS", &[sig("float_expression", "float")], "Arccosine in radians of a number from -1 to 1."),
    scalar(
        "APP_NAME",
        &[sig("", "nvarchar(128)")],
        "Application name of the current session, if the application set one.",
    ),
    aggregate(
        "APPROX_COUNT_DISTINCT",
        &[sig("expression", "bigint")],
        "Approximate number of distinct non-null values in a group.",
    )
    .since("SQL Server 2019"),
    aggregate(
        "APPROX_PERCENTILE_CONT",
        &[sig("percentile) WITHIN GROUP (ORDER BY order_by_expression [ASC | DESC]", "float(53)")],
        "Approximate interpolated percentile of the values in a group.",
    )
    .since("SQL Server 2022"),
    aggregate(
        "APPROX_PERCENTILE_DISC",
        &[sig(
            "percentile) WITHIN GROUP (ORDER BY order_by_expression [ASC | DESC]",
            "same type as order_by_expression",
        )],
        "Approximate percentile of a group, picked from its actual values.",
    )
    .since("SQL Server 2022"),
    scalar("ASCII", &[sig("character_expression", "int")], "Character code of the leftmost character of a string."),
    scalar("ASIN", &[sig("float_expression", "float")], "Arcsine in radians of a number from -1 to 1."),
    scalar("ATAN", &[sig("float_expression", "float")], "Arctangent in radians of a number."),
    scalar("ATN2", &[sig("y, x", "float")], "Arctangent in radians of y / x, using both signs to pick the quadrant."),
    aggregate(
        "AVG",
        &[sig("[ALL | DISTINCT] expression", "int, bigint, decimal, money or float")],
        "Average of the non-null values in a group.",
    ),
    scalar("BASE64_DECODE", &[sig("expression", "varbinary")], "Decodes Base64 text into binary data.")
        .since("SQL Server 2025"),
    scalar(
        "BASE64_ENCODE",
        &[sig("expression [, url_safe]", "varchar")],
        "Encodes binary data as Base64 text, optionally URL-safe.",
    )
    .since("SQL Server 2025"),
    scalar(
        "BINARY_CHECKSUM",
        &[sig("* | expression [, ...n]", "int")],
        "Checksum over the binary values of a row or expressions, for detecting changes.",
    ),
    scalar("BIT_COUNT", &[sig("expression_value", "bigint")], "Number of bits set to 1 in an integer or binary value.")
        .since("SQL Server 2022"),
    scalar("CAST", &[sig("expression AS data_type [(length)]", "data_type")], "Converts a value to another data type.")
        .flags(SYNTAX),
    scalar(
        "CEILING",
        &[sig("numeric_expression", "same type as numeric_expression")],
        "Smallest integer greater than or equal to a number.",
    ),
    scalar("CHAR", &[sig("integer_expression", "char(1)")], "Single-byte character with the given character code."),
    scalar(
        "CHARINDEX",
        &[sig("expressionToFind, expressionToSearch [, start_location]", "int or bigint")],
        "Position of the first occurrence of a substring in a string, or 0 if not found.",
    ),
    scalar(
        "CHECKSUM",
        &[sig("* | expression [, ...n]", "int")],
        "Hash value computed over a row or a list of expressions.",
    ),
    aggregate(
        "CHECKSUM_AGG",
        &[sig("[ALL | DISTINCT] expression", "int")],
        "Checksum of the values in a group, for detecting changes.",
    ),
    scalar(
        "CHOOSE",
        &[sig("index, val_1, val_2 [, val_n]", "highest-precedence argument type")],
        "Value at the given 1-based index in a list of values.",
    ),
    scalar(
        "COALESCE",
        &[sig("expression [, ...n]", "highest-precedence argument type")],
        "First argument that isn't NULL.",
    )
    .flags(SYNTAX),
    scalar("COL_LENGTH", &[sig("table, column", "smallint")], "Defined length of a column, in bytes."),
    scalar("COL_NAME", &[sig("table_id, column_id", "sysname")], "Name of a column from its table ID and column ID."),
    scalar(
        "COLLATIONPROPERTY",
        &[sig("collation_name, property", "sql_variant")],
        "Property of a collation, such as its code page or comparison style.",
    ),
    scalar(
        "COLUMNPROPERTY",
        &[sig("id, column, property", "int")],
        "Property of a column or procedure parameter, such as AllowsNull.",
    ),
    scalar(
        "COLUMNS_UPDATED",
        &[sig("", "varbinary")],
        "Bit pattern of the columns inserted or updated, inside a DML trigger.",
    ),
    scalar("COMPRESS", &[sig("expression", "varbinary(max)")], "Compresses a value with the Gzip algorithm."),
    scalar(
        "CONCAT",
        &[sig("argument1, argument2 [, ...argumentN]", "varchar or nvarchar")],
        "Joins two or more values into one string, treating NULL as an empty string.",
    ),
    scalar(
        "CONCAT_WS",
        &[sig("separator, argument1, argument2 [, ...argumentN]", "varchar or nvarchar")],
        "Joins two or more values with a separator, skipping NULLs.",
    )
    .since("SQL Server 2017"),
    scalar(
        "CONNECTIONPROPERTY",
        &[sig("property", "sql_variant")],
        "Property of the current connection, such as its protocol or client address.",
    ),
    table(
        "CONTAINSTABLE",
        &[sig(
            "table, {column_name | (column_list) | *}, contains_search_condition [, LANGUAGE language_term] [, top_n_by_rank]",
            "table (KEY, RANK)",
        )],
        "Full-text search returning the key and rank of each matching row.",
    ),
    scalar(
        "CONTEXT_INFO",
        &[sig("", "varbinary(128)")],
        "Context information set for the session with SET CONTEXT_INFO.",
    ),
    scalar(
        "CONVERT",
        &[sig("data_type [(length)], expression [, style]", "data_type")],
        "Converts a value to another data type, with an optional format style.",
    ),
    scalar("COS", &[sig("float_expression", "float")], "Cosine of an angle in radians."),
    scalar("COT", &[sig("float_expression", "float")], "Cotangent of an angle in radians."),
    aggregate(
        "COUNT",
        &[sig("*", "int"), sig("[ALL | DISTINCT] expression", "int")],
        "Number of rows, or of non-null values, in a group.",
    ),
    aggregate(
        "COUNT_BIG",
        &[sig("*", "bigint"), sig("[ALL | DISTINCT] expression", "bigint")],
        "Number of rows, or of non-null values, in a group, as a bigint.",
    ),
    scalar(
        "CRYPT_GEN_RANDOM",
        &[sig("length [, seed]", "varbinary(8000)")],
        "Cryptographically random bytes of the given length.",
    ),
    window("CUME_DIST", &[sig("", "float(53)")], "Cumulative distribution of the current row within its partition."),
    scalar("CURRENT_TIMESTAMP", &[sig("", "datetime")], "Current date and time of the server, like GETDATE().")
        .flags(NO_PARENS),
    scalar("CURRENT_USER", &[sig("", "sysname")], "Name of the current database user.").flags(NO_PARENS),
    scalar(
        "CURSOR_STATUS",
        &[sig("{'local' | 'global'}, cursor_name", "smallint"), sig("'variable', cursor_variable", "smallint")],
        "Whether a cursor is open with rows, open and empty, closed or missing.",
    ),
    scalar(
        "DATABASE_PRINCIPAL_ID",
        &[sig("[principal_name]", "int")],
        "ID of a database principal, or of the current user.",
    ),
    scalar(
        "DATABASEPROPERTYEX",
        &[sig("database, property", "sql_variant")],
        "Property of a database, such as its status, recovery model or collation.",
    ),
    scalar("DATALENGTH", &[sig("expression", "int or bigint")], "Number of bytes used to store a value."),
    scalar(
        "DATE_BUCKET",
        &[sig("datepart, number, date [, origin]", "same type as date")],
        "Start of the fixed-width date bucket that contains a date.",
    )
    .since("SQL Server 2022"),
    scalar("DATEADD", &[sig("datepart, number, date", "same type as date")], "Adds a number of dateparts to a date."),
    scalar(
        "DATEDIFF",
        &[sig("datepart, startdate, enddate", "int")],
        "Number of datepart boundaries crossed between two dates.",
    ),
    scalar(
        "DATEDIFF_BIG",
        &[sig("datepart, startdate, enddate", "bigint")],
        "Number of datepart boundaries crossed between two dates, as a bigint.",
    ),
    scalar("DATEFROMPARTS", &[sig("year, month, day", "date")], "Builds a date from a year, month and day."),
    scalar(
        "DATENAME",
        &[sig("datepart, date", "nvarchar")],
        "Name of a part of a date, such as the month or weekday name.",
    ),
    scalar("DATEPART", &[sig("datepart, date", "int")], "Integer value of a part of a date, such as the year or hour."),
    scalar(
        "DATETIME2FROMPARTS",
        &[sig("year, month, day, hour, minute, seconds, fractions, precision", "datetime2")],
        "Builds a datetime2 value from its parts.",
    ),
    scalar(
        "DATETIMEFROMPARTS",
        &[sig("year, month, day, hour, minute, seconds, milliseconds", "datetime")],
        "Builds a datetime value from its parts.",
    ),
    scalar(
        "DATETIMEOFFSETFROMPARTS",
        &[sig(
            "year, month, day, hour, minute, seconds, fractions, hour_offset, minute_offset, precision",
            "datetimeoffset",
        )],
        "Builds a datetimeoffset value from its parts and a time zone offset.",
    ),
    scalar(
        "DATETRUNC",
        &[sig("datepart, date", "same type as date")],
        "Truncates a date to the start of the given datepart.",
    )
    .since("SQL Server 2022"),
    scalar("DAY", &[sig("date", "int")], "Day of the month of a date."),
    scalar("DB_ID", &[sig("[database_name]", "int")], "ID of a database, or of the current one."),
    scalar("DB_NAME", &[sig("[database_id]", "nvarchar(128)")], "Name of a database, or of the current one."),
    scalar(
        "DECOMPRESS",
        &[sig("expression", "varbinary(max)")],
        "Decompresses a value compressed with the Gzip algorithm.",
    ),
    scalar(
        "DECRYPTBYKEY",
        &[sig("ciphertext [, add_authenticator, authenticator]", "varbinary(8000)")],
        "Decrypts data with a symmetric key that is open in the session.",
    ),
    scalar(
        "DECRYPTBYPASSPHRASE",
        &[sig("passphrase, ciphertext [, add_authenticator, authenticator]", "varbinary(8000)")],
        "Decrypts data that was encrypted with a passphrase.",
    ),
    scalar(
        "DEGREES",
        &[sig("numeric_expression", "same type as numeric_expression")],
        "Converts an angle from radians to degrees.",
    ),
    window("DENSE_RANK", &[sig("", "bigint")], "Rank of the current row within its partition, without gaps."),
    scalar(
        "DIFFERENCE",
        &[sig("character_expression, character_expression", "int")],
        "Similarity of the SOUNDEX codes of two strings, from 0 to 4.",
    ),
    scalar(
        "ENCRYPTBYKEY",
        &[sig("key_GUID, cleartext [, add_authenticator, authenticator]", "varbinary(8000)")],
        "Encrypts data with an open symmetric key, identified by its GUID.",
    ),
    scalar(
        "ENCRYPTBYPASSPHRASE",
        &[sig("passphrase, cleartext [, add_authenticator, authenticator]", "varbinary(8000)")],
        "Encrypts data with a key derived from a passphrase.",
    ),
    scalar(
        "EOMONTH",
        &[sig("start_date [, month_to_add]", "date")],
        "Last day of the month of a date, optionally moved by some months.",
    ),
    scalar("ERROR_LINE", &[sig("", "int")], "Line number at which the error caught by a CATCH block occurred."),
    scalar("ERROR_MESSAGE", &[sig("", "nvarchar(4000)")], "Message text of the error caught by a CATCH block."),
    scalar("ERROR_NUMBER", &[sig("", "int")], "Number of the error caught by a CATCH block."),
    scalar(
        "ERROR_PROCEDURE",
        &[sig("", "nvarchar(128)")],
        "Procedure or trigger in which the error caught by a CATCH block occurred.",
    ),
    scalar("ERROR_SEVERITY", &[sig("", "int")], "Severity of the error caught by a CATCH block."),
    scalar("ERROR_STATE", &[sig("", "int")], "State number of the error caught by a CATCH block."),
    scalar("EVENTDATA", &[sig("", "xml")], "Details of the event that fired a DDL or logon trigger, as XML."),
    scalar("EXP", &[sig("float_expression", "float")], "Exponential of a number, e raised to that power."),
    scalar("FILE_NAME", &[sig("file_id", "nvarchar(128)")], "Logical name of a database file from its ID."),
    scalar("FILEGROUP_NAME", &[sig("filegroup_id", "nvarchar(128)")], "Name of a filegroup from its ID."),
    scalar(
        "FILEPROPERTY",
        &[sig("file_name, property", "int")],
        "Property of a database file, such as SpaceUsed in pages.",
    ),
    window(
        "FIRST_VALUE",
        &[sig("scalar_expression", "same type as scalar_expression")],
        "Value from the first row of the window frame.",
    ),
    scalar(
        "FLOOR",
        &[sig("numeric_expression", "same type as numeric_expression")],
        "Largest integer less than or equal to a number.",
    ),
    scalar(
        "FORMAT",
        &[sig("value, format [, culture]", "nvarchar")],
        "Formats a date, time or number with a .NET format string and optional culture.",
    ),
    scalar(
        "FORMATMESSAGE",
        &[sig("{msg_number | msg_string} [, param_value [, ...n]]", "nvarchar")],
        "Builds a message from sys.messages or a format string and parameters.",
    ),
    table(
        "FREETEXTTABLE",
        &[sig(
            "table, {column_name | (column_list) | *}, freetext_string [, LANGUAGE language_term] [, top_n_by_rank]",
            "table (KEY, RANK)",
        )],
        "Full-text search by meaning, returning the key and rank of each matching row.",
    ),
    table(
        "GENERATE_SERIES",
        &[sig("start, stop [, step]", "table (value)")],
        "Rows of numbers from start to stop, in increments of step.",
    )
    .since("SQL Server 2022"),
    scalar(
        "GET_BIT",
        &[sig("expression_value, bit_offset", "bit")],
        "Bit at the given offset of an integer or binary value.",
    )
    .since("SQL Server 2022"),
    scalar("GETDATE", &[sig("", "datetime")], "Current date and time of the server."),
    scalar("GETUTCDATE", &[sig("", "datetime")], "Current UTC date and time of the server."),
    scalar(
        "GREATEST",
        &[sig("expression1 [, ...expressionN]", "highest-precedence argument type")],
        "Largest of the arguments, ignoring NULLs.",
    )
    .since("SQL Server 2022"),
    aggregate(
        "GROUPING",
        &[sig("column_expression", "tinyint")],
        "Tests whether a GROUP BY column is aggregated in the current result row.",
    ),
    aggregate(
        "GROUPING_ID",
        &[sig("column_expression [, ...n]", "int")],
        "Bitmask of the GROUP BY columns aggregated in the current result row.",
    ),
    scalar("HAS_DBACCESS", &[sig("database_name", "int")], "Tests whether the current user can access a database."),
    scalar(
        "HAS_PERMS_BY_NAME",
        &[sig("securable, securable_class, permission [, sub_securable] [, sub_securable_class]", "int")],
        "Tests whether the current user has a permission on a securable.",
    ),
    scalar(
        "HASHBYTES",
        &[sig("algorithm, input", "varbinary(8000)")],
        "Hash of the input with an algorithm such as 'SHA2_256' or 'MD5'.",
    ),
    scalar("HOST_NAME", &[sig("", "nvarchar(128)")], "Workstation name the client reported when it connected."),
    scalar(
        "IDENT_CURRENT",
        &[sig("table_or_view", "numeric(38, 0)")],
        "Last identity value generated for a table, in any session and scope.",
    ),
    scalar(
        "IDENT_INCR",
        &[sig("table_or_view", "numeric(38, 0)")],
        "Increment of the identity column of a table or view.",
    ),
    scalar("IDENT_SEED", &[sig("table_or_view", "numeric(38, 0)")], "Seed of the identity column of a table or view."),
    scalar(
        "IIF",
        &[sig("boolean_expression, true_value, false_value", "highest-precedence argument type")],
        "One of two values, depending on whether a condition is true.",
    ),
    scalar(
        "INDEX_COL",
        &[sig("table_or_view_name, index_id, key_id", "nvarchar(128)")],
        "Name of a key column of an index.",
    ),
    scalar(
        "INDEXPROPERTY",
        &[sig("object_id, index_or_statistics_name, property", "int")],
        "Property of an index or statistics, such as IsClustered.",
    ),
    scalar(
        "IS_MEMBER",
        &[sig("{group | role}", "int")],
        "Tests whether the current user belongs to a Windows group or database role.",
    ),
    scalar(
        "IS_ROLEMEMBER",
        &[sig("role [, database_principal]", "int")],
        "Tests whether a database principal belongs to a database role.",
    ),
    scalar("IS_SRVROLEMEMBER", &[sig("role [, login]", "int")], "Tests whether a login belongs to a server role."),
    scalar(
        "ISDATE",
        &[sig("expression", "int")],
        "Tests whether an expression is a valid date, time or datetime value.",
    ),
    scalar(
        "ISJSON",
        &[sig("expression [, json_type_constraint]", "int")],
        "Tests whether a string is valid JSON, optionally of a given type on SQL Server 2022.",
    ),
    scalar(
        "ISNULL",
        &[sig("check_expression, replacement_value", "same type as check_expression")],
        "Replaces NULL with a replacement value.",
    ),
    scalar("ISNUMERIC", &[sig("expression", "int")], "Tests whether an expression converts to a numeric type."),
    scalar(
        "JSON_ARRAY",
        &[sig("[value [, ...n]] [{NULL | ABSENT} ON NULL]", "nvarchar(max)")],
        "Builds a JSON array from zero or more values.",
    )
    .since("SQL Server 2022"),
    aggregate(
        "JSON_ARRAYAGG",
        &[sig("value_expression [ORDER BY order_by_expression] [{NULL | ABSENT} ON NULL]", "nvarchar(max)")],
        "Builds a JSON array from the values of a group.",
    )
    .since("SQL Server 2025"),
    scalar(
        "JSON_MODIFY",
        &[sig("expression, path, newValue", "nvarchar(max)")],
        "Updates a value in a JSON string and returns the new JSON.",
    ),
    scalar(
        "JSON_OBJECT",
        &[sig("[key : value [, ...n]] [{NULL | ABSENT} ON NULL]", "nvarchar(max)")],
        "Builds a JSON object from zero or more key-value pairs.",
    )
    .since("SQL Server 2022"),
    aggregate(
        "JSON_OBJECTAGG",
        &[sig("key : value [{NULL | ABSENT} ON NULL]", "nvarchar(max)")],
        "Builds a JSON object from the key-value pairs of a group.",
    )
    .since("SQL Server 2025"),
    scalar(
        "JSON_PATH_EXISTS",
        &[sig("value_expression, sql_json_path", "int")],
        "Tests whether a SQL/JSON path exists in a JSON string.",
    )
    .since("SQL Server 2022"),
    scalar(
        "JSON_QUERY",
        &[sig("expression [, path]", "nvarchar(max)")],
        "Extracts an object or array from a JSON string.",
    ),
    scalar("JSON_VALUE", &[sig("expression, path", "nvarchar(4000)")], "Extracts a scalar value from a JSON string."),
    scalar("KEY_GUID", &[sig("key_name", "uniqueidentifier")], "GUID of a symmetric key in the database."),
    window(
        "LAG",
        &[sig("scalar_expression [, offset [, default]]", "same type as scalar_expression")],
        "Value from a row that comes a given number of rows before the current one.",
    ),
    window(
        "LAST_VALUE",
        &[sig("scalar_expression", "same type as scalar_expression")],
        "Value from the last row of the window frame.",
    ),
    window(
        "LEAD",
        &[sig("scalar_expression [, offset [, default]]", "same type as scalar_expression")],
        "Value from a row that comes a given number of rows after the current one.",
    ),
    scalar(
        "LEAST",
        &[sig("expression1 [, ...expressionN]", "highest-precedence argument type")],
        "Smallest of the arguments, ignoring NULLs.",
    )
    .since("SQL Server 2022"),
    scalar(
        "LEFT",
        &[sig("character_expression, integer_expression", "varchar or nvarchar")],
        "Leftmost characters of a string.",
    ),
    scalar(
        "LEFT_SHIFT",
        &[sig("expression_value, shift_amount", "same type as expression_value")],
        "Shifts the bits of an integer or binary value to the left.",
    )
    .since("SQL Server 2022"),
    scalar(
        "LEN",
        &[sig("string_expression", "int or bigint")],
        "Number of characters in a string, excluding trailing spaces.",
    ),
    scalar(
        "LOG",
        &[sig("float_expression [, base]", "float")],
        "Natural logarithm of a number, or its logarithm in the given base.",
    ),
    scalar("LOG10", &[sig("float_expression", "float")], "Base-10 logarithm of a number."),
    scalar("LOWER", &[sig("character_expression", "varchar or nvarchar")], "Converts a string to lowercase."),
    scalar(
        "LTRIM",
        &[sig("character_expression [, characters]", "varchar or nvarchar")],
        "Removes leading spaces, or leading characters from a given set (SQL Server 2022).",
    ),
    aggregate("MAX", &[sig("[ALL | DISTINCT] expression", "same type as expression")], "Largest value in a group."),
    aggregate("MIN", &[sig("[ALL | DISTINCT] expression", "same type as expression")], "Smallest value in a group."),
    scalar("MONTH", &[sig("date", "int")], "Month of a date, from 1 to 12."),
    scalar("NCHAR", &[sig("integer_expression", "nchar or nvarchar")], "Unicode character with the given code point."),
    scalar("NEWID", &[sig("", "uniqueidentifier")], "New random uniqueidentifier value."),
    scalar(
        "NEWSEQUENTIALID",
        &[sig("", "uniqueidentifier")],
        "Increasing uniqueidentifier value, usable only in a DEFAULT constraint.",
    ),
    window(
        "NTILE",
        &[sig("integer_expression", "bigint")],
        "Number of the group a row falls into when its partition is split into n groups.",
    ),
    scalar(
        "NULLIF",
        &[sig("expression, expression", "same type as the first expression")],
        "NULL if the two expressions are equal, otherwise the first one.",
    )
    .flags(SYNTAX),
    scalar(
        "OBJECT_DEFINITION",
        &[sig("object_id", "nvarchar(max)")],
        "Source text of a view, procedure, function, trigger or other module.",
    ),
    scalar(
        "OBJECT_ID",
        &[sig("object_name [, object_type]", "int")],
        "ID of a schema-scoped object by name, optionally of a given type.",
    ),
    scalar(
        "OBJECT_NAME",
        &[sig("object_id [, database_id]", "sysname")],
        "Name of a schema-scoped object from its ID.",
    ),
    scalar(
        "OBJECT_SCHEMA_NAME",
        &[sig("object_id [, database_id]", "sysname")],
        "Schema name of a schema-scoped object from its ID.",
    ),
    scalar("OBJECTPROPERTY", &[sig("id, property", "int")], "Property of a schema-scoped object, such as IsUserTable."),
    scalar(
        "OBJECTPROPERTYEX",
        &[sig("id, property", "sql_variant")],
        "Property of a schema-scoped object, including non-integer ones such as BaseType.",
    ),
    table("OPENJSON", &[sig("jsonExpression [, path]", "table")], "Parses JSON text into rows and columns."),
    table("OPENQUERY", &[sig("linked_server, query", "table")], "Rows of a pass-through query run on a linked server."),
    table(
        "OPENROWSET",
        &[
            sig("provider_name, {datasource; user_id; password | provider_string}, {object | query}", "table"),
            sig(
                "BULK data_file, {FORMATFILE = format_file_path [, options] | SINGLE_BLOB | SINGLE_CLOB | SINGLE_NCLOB}",
                "table",
            ),
        ],
        "Rows from an OLE DB data source, or with BULK, from a file.",
    ),
    table(
        "OPENXML",
        &[sig("idoc, rowpattern [, flags]", "table")],
        "Rowset view of an XML document prepared with sp_xml_preparedocument.",
    ),
    scalar("ORIGINAL_DB_NAME", &[sig("", "nvarchar(128)")], "Database name given in the connection string."),
    scalar(
        "ORIGINAL_LOGIN",
        &[sig("", "sysname")],
        "Login that first connected to the session, even after impersonation.",
    ),
    scalar(
        "PARSE",
        &[sig("string_value AS data_type [USING culture]", "data_type")],
        "Converts a string to a date, time or number type using a .NET culture.",
    ),
    scalar(
        "PARSENAME",
        &[sig("object_name, object_piece", "sysname")],
        "One part of a multipart object name, counted from the right.",
    ),
    scalar(
        "PATINDEX",
        &[sig("pattern, expression", "int or bigint")],
        "Position of the first match of a LIKE pattern in a string, or 0 if none.",
    ),
    window(
        "PERCENT_RANK",
        &[sig("", "float(53)")],
        "Relative rank of the current row within its partition, from 0 to 1.",
    ),
    window(
        "PERCENTILE_CONT",
        &[sig("percentile) WITHIN GROUP (ORDER BY order_by_expression [ASC | DESC]", "float(53)")],
        "Interpolated percentile of the values in a partition.",
    ),
    window(
        "PERCENTILE_DISC",
        &[sig(
            "percentile) WITHIN GROUP (ORDER BY order_by_expression [ASC | DESC]",
            "same type as order_by_expression",
        )],
        "Percentile of a partition, picked from its actual values.",
    ),
    scalar("PI", &[sig("", "float")], "The constant pi."),
    scalar(
        "POWER",
        &[sig("float_expression, y", "same type as float_expression")],
        "A number raised to the given power.",
    ),
    aggregate(
        "PRODUCT",
        &[sig("[ALL | DISTINCT] expression", "int, bigint, decimal, money or float")],
        "Product of the non-null values in a group.",
    )
    .since("SQL Server 2025"),
    scalar(
        "QUOTENAME",
        &[sig("character_string [, quote_character]", "nvarchar(258)")],
        "Adds delimiters to a string to make it a valid delimited identifier.",
    ),
    scalar(
        "RADIANS",
        &[sig("numeric_expression", "same type as numeric_expression")],
        "Converts an angle from degrees to radians.",
    ),
    scalar("RAND", &[sig("[seed]", "float")], "Pseudo-random float from 0 up to but not including 1."),
    window("RANK", &[sig("", "bigint")], "Rank of the current row within its partition, with gaps."),
    scalar(
        "REGEXP_COUNT",
        &[sig("string_expression, pattern_expression [, start [, flags]]", "int")],
        "Number of times a regular expression matches in a string.",
    )
    .since("SQL Server 2025"),
    scalar(
        "REGEXP_INSTR",
        &[sig(
            "string_expression, pattern_expression [, start [, occurrence [, return_option [, flags [, group]]]]]",
            "int",
        )],
        "Position where a match of a regular expression starts or ends.",
    )
    .since("SQL Server 2025"),
    scalar(
        "REGEXP_LIKE",
        &[sig("string_expression, pattern_expression [, flags]", "boolean")],
        "Tests whether a string matches a regular expression.",
    )
    .since("SQL Server 2025"),
    table(
        "REGEXP_MATCHES",
        &[sig("string_expression, pattern_expression [, flags]", "table")],
        "Rows describing each match of a regular expression in a string.",
    )
    .since("SQL Server 2025"),
    scalar(
        "REGEXP_REPLACE",
        &[sig(
            "string_expression, pattern_expression [, string_replacement [, start [, occurrence [, flags]]]]",
            "varchar or nvarchar",
        )],
        "Replaces matches of a regular expression in a string.",
    )
    .since("SQL Server 2025"),
    table(
        "REGEXP_SPLIT_TO_TABLE",
        &[sig("string_expression, pattern_expression [, flags]", "table")],
        "Splits a string into rows at each match of a regular expression.",
    )
    .since("SQL Server 2025"),
    scalar(
        "REGEXP_SUBSTR",
        &[sig(
            "string_expression, pattern_expression [, start [, occurrence [, flags [, group]]]]",
            "varchar or nvarchar",
        )],
        "Substring that matches a regular expression.",
    )
    .since("SQL Server 2025"),
    scalar(
        "REPLACE",
        &[sig("string_expression, string_pattern, string_replacement", "varchar or nvarchar")],
        "Replaces every occurrence of a substring with another string.",
    ),
    scalar(
        "REPLICATE",
        &[sig("string_expression, integer_expression", "same type as string_expression")],
        "Repeats a string a number of times.",
    ),
    scalar(
        "REVERSE",
        &[sig("string_expression", "varchar or nvarchar")],
        "Reverses the order of the characters in a string.",
    ),
    scalar(
        "RIGHT",
        &[sig("character_expression, integer_expression", "varchar or nvarchar")],
        "Rightmost characters of a string.",
    ),
    scalar(
        "RIGHT_SHIFT",
        &[sig("expression_value, shift_amount", "same type as expression_value")],
        "Shifts the bits of an integer or binary value to the right.",
    )
    .since("SQL Server 2022"),
    scalar(
        "ROUND",
        &[sig("numeric_expression, length [, function]", "same type as numeric_expression")],
        "Rounds a number to a given precision, or truncates it if function isn't 0.",
    ),
    window("ROW_NUMBER", &[sig("", "bigint")], "Sequential number of the current row within its partition, from 1."),
    scalar("ROWCOUNT_BIG", &[sig("", "bigint")], "Number of rows affected by the last statement, as a bigint."),
    scalar(
        "RTRIM",
        &[sig("character_expression [, characters]", "varchar or nvarchar")],
        "Removes trailing spaces, or trailing characters from a given set (SQL Server 2022).",
    ),
    scalar("SCHEMA_ID", &[sig("[schema_name]", "int")], "ID of a schema, or of the caller's default schema."),
    scalar("SCHEMA_NAME", &[sig("[schema_id]", "sysname")], "Name of a schema, or of the caller's default schema."),
    scalar("SCOPE_IDENTITY", &[sig("", "numeric(38, 0)")], "Last identity value inserted in the current scope."),
    scalar(
        "SERVERPROPERTY",
        &[sig("property", "sql_variant")],
        "Property of the server instance, such as ProductVersion or Edition.",
    ),
    scalar(
        "SESSION_CONTEXT",
        &[sig("key", "sql_variant")],
        "Value stored for a key in the session context by sp_set_session_context.",
    ),
    scalar("SESSION_USER", &[sig("", "nvarchar(128)")], "Name of the current database user in the current context.")
        .flags(NO_PARENS),
    scalar(
        "SESSIONPROPERTY",
        &[sig("option", "sql_variant")],
        "Setting of a SET option for the session, such as ANSI_NULLS.",
    ),
    scalar(
        "SET_BIT",
        &[sig("expression_value, bit_offset [, bit_value]", "same type as expression_value")],
        "Sets the bit at an offset of an integer or binary value to 1, or to bit_value.",
    )
    .since("SQL Server 2022"),
    scalar("SIGN", &[sig("numeric_expression", "same type as numeric_expression")], "Sign of a number: -1, 0 or 1."),
    scalar("SIN", &[sig("float_expression", "float")], "Sine of an angle in radians."),
    scalar(
        "SMALLDATETIMEFROMPARTS",
        &[sig("year, month, day, hour, minute", "smalldatetime")],
        "Builds a smalldatetime value from its parts.",
    ),
    scalar(
        "SOUNDEX",
        &[sig("character_expression", "varchar")],
        "Four-character code for how a string sounds, for matching similar names.",
    ),
    scalar("SPACE", &[sig("integer_expression", "varchar")], "String of the given number of spaces."),
    scalar(
        "SQL_VARIANT_PROPERTY",
        &[sig("expression, property", "sql_variant")],
        "Base type or another property of a sql_variant value.",
    ),
    scalar("SQRT", &[sig("float_expression", "float")], "Square root of a number."),
    scalar("SQUARE", &[sig("float_expression", "float")], "Square of a number."),
    scalar(
        "STATS_DATE",
        &[sig("object_id, stats_id", "datetime")],
        "When statistics on a table or indexed view were last updated.",
    ),
    aggregate(
        "STDEV",
        &[sig("[ALL | DISTINCT] expression", "float")],
        "Sample standard deviation of the values in a group.",
    ),
    aggregate(
        "STDEVP",
        &[sig("[ALL | DISTINCT] expression", "float")],
        "Population standard deviation of the values in a group.",
    ),
    scalar(
        "STR",
        &[sig("float_expression [, length [, decimal]]", "varchar")],
        "Converts a number to right-aligned text with a given length and decimals.",
    ),
    aggregate(
        "STRING_AGG",
        &[
            sig("expression, separator", "varchar or nvarchar"),
            sig(
                "expression, separator) WITHIN GROUP (ORDER BY order_by_expression [ASC | DESC]",
                "varchar or nvarchar",
            ),
        ],
        "Joins the values of a group into one string with a separator.",
    )
    .since("SQL Server 2017"),
    scalar(
        "STRING_ESCAPE",
        &[sig("text, type", "varchar(max) or nvarchar(max)")],
        "Escapes special and control characters in text, for type 'json'.",
    ),
    table(
        "STRING_SPLIT",
        &[sig("string, separator [, enable_ordinal]", "table (value [, ordinal])")],
        "Splits a string into rows at a separator, with ordinals on SQL Server 2022.",
    ),
    scalar(
        "STUFF",
        &[sig("character_expression, start, length, replace_with_expression", "varchar, nvarchar or varbinary")],
        "Deletes part of a string and inserts another string in its place.",
    ),
    scalar(
        "SUBSTRING",
        &[sig("expression, start, length", "varchar, nvarchar or varbinary")],
        "Part of a character, binary, text or image value.",
    ),
    aggregate(
        "SUM",
        &[sig("[ALL | DISTINCT] expression", "int, bigint, decimal, money or float")],
        "Sum of the values in a group.",
    ),
    scalar(
        "SUSER_NAME",
        &[sig("[server_user_id]", "nvarchar(128)")],
        "Login name for a login ID, or of the current login.",
    ),
    scalar(
        "SUSER_SID",
        &[sig("[login] [, param2]", "varbinary(85)")],
        "Security identifier of a login, or of the current login.",
    ),
    scalar(
        "SUSER_SNAME",
        &[sig("[server_user_sid]", "nvarchar(128)")],
        "Login name for a security identifier, or of the current login.",
    ),
    scalar(
        "SWITCHOFFSET",
        &[sig("datetimeoffset_expression, timezoneoffset_expression", "datetimeoffset")],
        "Converts a datetimeoffset value to another time zone offset.",
    ),
    scalar("SYSDATETIME", &[sig("", "datetime2(7)")], "Current date and time of the server, with high precision."),
    scalar(
        "SYSDATETIMEOFFSET",
        &[sig("", "datetimeoffset(7)")],
        "Current date and time of the server, with its time zone offset.",
    ),
    scalar("SYSTEM_USER", &[sig("", "nvarchar(128)")], "Login name of the current session.").flags(NO_PARENS),
    scalar(
        "SYSUTCDATETIME",
        &[sig("", "datetime2(7)")],
        "Current UTC date and time of the server, with high precision.",
    ),
    scalar("TAN", &[sig("float_expression", "float")], "Tangent of an angle in radians."),
    scalar(
        "TIMEFROMPARTS",
        &[sig("hour, minute, seconds, fractions, precision", "time")],
        "Builds a time value from its parts.",
    ),
    scalar(
        "TODATETIMEOFFSET",
        &[sig("datetime_expression, timezoneoffset_expression", "datetimeoffset")],
        "Pairs a date and time with a time zone offset, as a datetimeoffset value.",
    ),
    scalar(
        "TRANSLATE",
        &[sig("inputString, characters, translations", "same type as inputString")],
        "Replaces each character from one set with the matching character of another.",
    )
    .since("SQL Server 2017"),
    scalar(
        "TRIGGER_NESTLEVEL",
        &[sig("[object_id] [, trigger_type] [, trigger_event_category]", "int")],
        "Number of triggers that ran for the statement that fired the current trigger.",
    ),
    scalar(
        "TRIM",
        &[
            sig("[characters FROM] string", "varchar or nvarchar"),
            sig("{LEADING | TRAILING | BOTH} [characters FROM] string", "varchar or nvarchar"),
        ],
        "Removes spaces or given characters from both ends, or one end on SQL Server 2022.",
    )
    .since("SQL Server 2017"),
    scalar(
        "TRY_CAST",
        &[sig("expression AS data_type [(length)]", "data_type")],
        "Converts a value to another data type, or returns NULL if that fails.",
    )
    .flags(SYNTAX),
    scalar(
        "TRY_CONVERT",
        &[sig("data_type [(length)], expression [, style]", "data_type")],
        "Converts a value to another data type with a style, or NULL if that fails.",
    ),
    scalar(
        "TRY_PARSE",
        &[sig("string_value AS data_type [USING culture]", "data_type")],
        "Parses a string into a date, time or number type, or NULL if that fails.",
    ),
    scalar("TYPE_ID", &[sig("[schema_name.]type_name", "int")], "ID of a data type by name."),
    scalar("TYPE_NAME", &[sig("type_id", "sysname")], "Name of a data type from its ID."),
    scalar(
        "TYPEPROPERTY",
        &[sig("type, property", "int")],
        "Property of a data type, such as Precision or AllowsNull.",
    ),
    scalar("UNICODE", &[sig("ncharacter_expression", "int")], "Unicode code point of the first character of a string."),
    scalar(
        "UNISTR",
        &[sig("character_expression [, unicode_escape_character]", "nvarchar")],
        "Converts Unicode escape sequences in a string into the characters they encode.",
    )
    .since("SQL Server 2025"),
    scalar("UPPER", &[sig("character_expression", "varchar or nvarchar")], "Converts a string to uppercase."),
    scalar("USER", &[sig("", "nvarchar(128)")], "Name of the current database user, often used as a column default.")
        .flags(NO_PARENS),
    scalar(
        "USER_ID",
        &[sig("[user]", "int")],
        "ID of a database user, or of the current user, replaced by DATABASE_PRINCIPAL_ID.",
    )
    .flags(DEPRECATED),
    scalar("USER_NAME", &[sig("[id]", "nvarchar(128)")], "Database user name for an ID, or of the current user."),
    aggregate("VAR", &[sig("[ALL | DISTINCT] expression", "float")], "Sample variance of the values in a group."),
    aggregate("VARP", &[sig("[ALL | DISTINCT] expression", "float")], "Population variance of the values in a group."),
    scalar(
        "VECTOR_DISTANCE",
        &[sig("distance_metric, vector1, vector2", "float")],
        "Distance between two vectors by the 'cosine', 'euclidean' or 'dot' metric.",
    )
    .since("SQL Server 2025"),
    scalar("VECTOR_NORM", &[sig("vector, norm_type", "float")], "Norm of a vector by 'norm1', 'norm2' or 'norminf'.")
        .since("SQL Server 2025"),
    scalar(
        "VECTOR_NORMALIZE",
        &[sig("vector, norm_type", "vector")],
        "Vector scaled to a length of 1 under the given norm type.",
    )
    .since("SQL Server 2025"),
    scalar(
        "XACT_STATE",
        &[sig("", "smallint")],
        "State of the session's transaction: 1 committable, -1 uncommittable, 0 none.",
    ),
    scalar("YEAR", &[sig("date", "int")], "Year of a date."),
];
