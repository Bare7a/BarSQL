// Turso's vector search: libSQL's functions plus the newer Turso engine's. Turso reads these after the SQLite pack.

use super::{BuiltinFn, scalar, sig, table};

pub(super) const PACK: &[BuiltinFn] = &[
    scalar(
        "libsql_vector_idx",
        &[sig("column [, setting, ...]", "blob")],
        "Marks a vector column to index in CREATE INDEX, with optional 'key=value' settings.",
    ),
    scalar(
        "vector",
        &[sig("value", "blob")],
        "Float32 vector from JSON array text or another vector; alias of vector32.",
    ),
    scalar("vector16", &[sig("value", "blob")], "Float16 vector from JSON array text or another vector."),
    scalar(
        "vector1bit",
        &[sig("value", "blob")],
        "Binary vector with one bit per dimension, from JSON array text or another vector.",
    ),
    scalar("vector32", &[sig("value", "blob")], "Float32 vector from JSON array text or another vector."),
    scalar(
        "vector32_sparse",
        &[sig("value", "blob")],
        "Sparse float32 vector that stores only the non-zero components.",
    ),
    scalar("vector64", &[sig("value", "blob")], "Float64 vector from JSON array text or another vector."),
    scalar("vector8", &[sig("value", "blob")], "8-bit quantized vector from JSON array text or another vector."),
    scalar(
        "vector_concat",
        &[sig("vector, vector [, ...]", "blob")],
        "Vector made by joining two or more vectors of the same type.",
    ),
    scalar(
        "vector_distance_cos",
        &[sig("a, b", "real")],
        "Cosine distance, 1 minus cosine similarity, between two vectors of the same type and size.",
    ),
    scalar(
        "vector_distance_dot",
        &[sig("a, b", "real")],
        "Negative dot product of two vectors; smaller means more similar.",
    ),
    scalar(
        "vector_distance_jaccard",
        &[sig("a, b", "real")],
        "Jaccard distance between two vectors, best suited to 1-bit vectors.",
    ),
    scalar(
        "vector_distance_l2",
        &[sig("a, b", "real")],
        "Euclidean (L2) distance between two vectors of the same type and size.",
    ),
    scalar("vector_extract", &[sig("vector", "text")], "Text form of a vector as a JSON array."),
    scalar(
        "vector_slice",
        &[sig("vector, start, end", "blob")],
        "Part of a vector from start, inclusive, to end, exclusive, counting from 0.",
    ),
    table(
        "vector_top_k",
        &[sig("index_name, query_vector, k", "table")],
        "Row ids of the k approximate nearest neighbors of a vector, from a vector index.",
    ),
    scalar("vectorb16", &[sig("value", "blob")], "Bfloat16 vector from JSON array text or another vector."),
];
