/// Join an NR body and an LTE body into one source document. The constants and helpers below
/// are *bodies*, not documents: a merged document carries exactly one `version` marker, so
/// concatenating two complete documents would trip `duplicate \`version\``.
pub(super) fn source(nr_body: &str, lte_body: &str) -> String {
    format!("version 1\n{nr_body}\n{lte_body}\n")
}

pub(super) const MINIMAL_NR: &str = r#"
bc "LEGACY"

bf 715188856 {
    c "LEGACY"
}
"#;

pub(super) const MINIMAL_LTE: &str = r#"
f "400907661" fp=862505271 bm=4082165014
"#;

pub(super) fn profiled_nr(profile_key: &str) -> String {
    format!(
        r#"
bc "LEGACY"

bf 715188856 {{
    c "LEGACY"
}}

c "PROFILED" pi=7 sg=1 t="main" {{
    pf "{profile_key}" x=66813533 u=0
}}
"#
    )
}

pub(super) fn lte_with_file_key(file_key: &str) -> String {
    format!(
        r#"
f "{file_key}" fp=862505271 bm=4082165014
"#
    )
}

pub(super) fn nr_with_carrier_sections(sections: &str) -> String {
    format!(
        r#"
bc "LEGACY"

bf 715188856 {{
    c "LEGACY"
}}

{sections}
"#
    )
}

pub(super) fn nr_with_complete_domain() -> String {
    nr_with_carrier_sections(
        r#"
c "PROFILED" pi=7 sg=1 t="main" {
    pf "66813533" x=66813533 u=0
    pf "8969" x=8969 u=0
}

c "MAPPING" mi=8 {
    ps
}
"#,
    )
}

pub(super) fn lte_with_complete_domain() -> String {
    r#"
f "400907661" fp=862505271 bm=1
f "564260317" fp=874888686 bm=2
"#
    .into()
}
