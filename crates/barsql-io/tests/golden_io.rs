use std::fs;
use std::path::PathBuf;

use barsql_core::DriverType;
use barsql_db::Cell;
use barsql_io::export::resolve_columns;
use barsql_io::{CsvOptions, EXPORT_FORMATS, csv_values, export_to_string, new_reader, preview_file};
use serde_json::{Value as Json, json};

fn fixture(rel: &str) -> Json {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden").join(rel);
    serde_json::from_str(&fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

fn assert_all(rel: &str, failures: Vec<String>) {
    assert!(failures.is_empty(), "{} {rel} case(s) differ from the fixtures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn csv_reader_matches_the_fixtures() {
    let mut failures = Vec::new();
    for case in fixture("corpus/csv_reader.json").as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let opts = CsvOptions {
            delimiter: case["comma"].as_str().unwrap().to_string(),
            trim_space: case["trim"].as_bool().unwrap(),
            ..Default::default()
        };
        let mut reader = new_reader(input.as_bytes(), &opts).unwrap();
        let mut records = Vec::new();
        while let Some(fields) = reader.read().unwrap() {
            records.push(Json::Array(fields.iter().map(|f| json!({"Value": f.value, "Quoted": f.quoted})).collect()));
        }
        let _ = csv_values;
        if Json::Array(records.clone()) != case["records"] {
            failures.push(format!(
                "{input:?} comma={:?} trim={}\n  want: {}\n  got:  {}",
                case["comma"],
                case["trim"],
                case["records"],
                Json::Array(records)
            ));
        }
    }
    assert_all("csv_reader", failures);
}

#[test]
fn csv_previews_match_the_fixtures() {
    let dir = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    for case in fixture("corpus/csv_preview.json").as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let path = dir.path().join(format!("{name}.csv"));
        fs::write(&path, case["content"].as_str().unwrap()).unwrap();
        let opts: CsvOptions = serde_json::from_value(case["options"].clone()).unwrap();
        for (driver_name, driver) in
            [("postgres", DriverType::Postgres), ("mysql", DriverType::MySql), ("sqlite", DriverType::Sqlite)]
        {
            let got = match preview_file(&path, &driver, &opts) {
                Ok(preview) => json!({ "preview": preview }),
                Err(err) => json!({ "error": err }),
            };
            // A null row list is an empty one.
            let mut want = case["previews"][driver_name].clone();
            if want["preview"]["rows"].is_null() && want["preview"].is_object() {
                want["preview"]["rows"] = json!([]);
            }
            if got != want {
                failures.push(format!("{name}/{driver_name}\n  want: {want}\n  got:  {got}"));
            }
        }
    }
    assert_all("csv_preview", failures);
}

fn cell(v: &Json) -> Cell<'_> {
    match v {
        Json::Null => Cell::Null,
        Json::Bool(b) => Cell::Bool(*b),
        Json::String(s) => Cell::Text(s),
        other => panic!("numbers are converted before: {other}"),
    }
}

#[test]
fn export_formats_match_the_fixtures() {
    let mut failures = Vec::new();
    for case in fixture("export/formats.json").as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let result = &case["result"];
        let columns: Vec<String> = serde_json::from_value(result["columns"].clone()).unwrap();
        let types: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(i, _)| result["columnTypes"].get(i).and_then(Json::as_str).unwrap_or("").to_string())
            .collect();
        let table = result["tableName"].as_str();
        let numbers: Vec<Vec<Option<String>>> = result["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                row.as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_f64().map(|f| ryu_js::Buffer::new().format(f).to_string()))
                    .collect()
            })
            .collect();
        let rows: Vec<&Vec<Json>> = result["rows"].as_array().unwrap().iter().map(|r| r.as_array().unwrap()).collect();
        let to_cells = |ri: usize, cols: &[usize]| -> Vec<Cell<'_>> {
            match rows.get(ri) {
                None => cols.iter().map(|_| Cell::Null).collect(),
                Some(row) => cols
                    .iter()
                    .map(|&c| match &numbers[ri][c] {
                        Some(text) => Cell::Number(text),
                        None => cell(&row[c]),
                    })
                    .collect(),
            }
        };
        let (cols, row_indices): (Vec<usize>, Vec<usize>) = match case["subset"].as_object() {
            None => ((0..columns.len()).collect(), (0..rows.len()).collect()),
            Some(subset) => {
                let wanted: Vec<String> = serde_json::from_value(subset["columns"].clone()).unwrap();
                (resolve_columns(&columns, &wanted), serde_json::from_value(subset["rowIndices"].clone()).unwrap())
            }
        };
        let names: Vec<String> = cols.iter().map(|&c| columns[c].clone()).collect();
        let col_types: Vec<String> = cols.iter().map(|&c| types[c].clone()).collect();
        for format in EXPORT_FORMATS {
            let got = export_to_string(
                format,
                &names,
                &col_types,
                table,
                None,
                row_indices.iter().map(|&ri| to_cells(ri, &cols)),
            );
            let want = case["outputs"][format.id()].as_str().unwrap();
            if got != want {
                failures.push(format!("{name}/{}\n  want: {want:?}\n  got:  {got:?}", format.id()));
            }
        }
    }
    assert_all("export formats", failures);
}
