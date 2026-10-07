// Functions every dialect has under the same name and meaning. Dialect packs override these entries.

use super::flags::SYNTAX;
use super::{BuiltinFn, aggregate, scalar, sig, window};

pub(super) const PACK: &[BuiltinFn] = &[
    scalar("abs", &[sig("x numeric", "numeric")], "Absolute value of a number."),
    aggregate("avg", &[sig("expression", "numeric")], "Average of the non-null input values."),
    scalar("cast", &[sig("expression AS type", "type")], "Converts a value to another type.").flags(SYNTAX),
    scalar("coalesce", &[sig("value [, ...]", "any")], "First argument that isn't null, or null if they all are."),
    scalar("concat", &[sig("value [, ...]", "text")], "Joins the arguments as text, skipping nulls."),
    aggregate("count", &[sig("*", "bigint"), sig("expression", "bigint")], "Number of rows, or of non-null values."),
    window("dense_rank", &[sig("", "bigint")], "Rank of the current row within its partition, without gaps."),
    window("first_value", &[sig("value", "any")], "Value from the first row of the window frame."),
    scalar("floor", &[sig("x numeric", "numeric")], "Largest integer not greater than the argument."),
    window("last_value", &[sig("value", "any")], "Value from the last row of the window frame."),
    scalar("lower", &[sig("text", "text")], "Converts text to lowercase."),
    scalar("ltrim", &[sig("text", "text")], "Removes leading spaces."),
    aggregate("max", &[sig("expression", "any")], "Largest non-null input value."),
    aggregate("min", &[sig("expression", "any")], "Smallest non-null input value."),
    scalar("nullif", &[sig("value1, value2", "any")], "Null if the arguments are equal, otherwise the first one."),
    window("rank", &[sig("", "bigint")], "Rank of the current row within its partition, with gaps."),
    scalar("replace", &[sig("text, from, to", "text")], "Replaces every occurrence of one substring with another."),
    scalar(
        "round",
        &[sig("x numeric [, digits int]", "numeric")],
        "Rounds to the nearest value with the given digits.",
    ),
    window("row_number", &[sig("", "bigint")], "Number of the current row within its partition, from 1."),
    scalar("rtrim", &[sig("text", "text")], "Removes trailing spaces."),
    scalar("substring", &[sig("text, start [, length]", "text")], "Part of the text from a start position."),
    aggregate("sum", &[sig("expression", "numeric")], "Sum of the non-null input values."),
    scalar("trim", &[sig("text", "text")], "Removes leading and trailing spaces."),
    scalar("upper", &[sig("text", "text")], "Converts text to uppercase."),
];
