// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;

use crate::formats::ooxml::{PackageError, XmlBudget, XmlElement, XmlEvent, parse_xml_events};

use super::error::ExcelError;
use super::limits::{
    MAX_CELL_RECORDS, MAX_COLUMN_RANGES_PER_SHEET, MAX_COMMENT_BYTES, MAX_COMMENTS,
    MAX_DEFINED_NAME_BYTES, MAX_DEFINED_NAMES, MAX_EXCEL_COLUMNS, MAX_EXCEL_ROWS,
    MAX_SHARED_STRING_BYTES, MAX_SHARED_STRING_ENTRIES, MAX_SHEET_NAME_BYTES, MAX_WORKBOOK_SHEETS,
};
use super::opc::optional_attribute;

pub(super) const SPREADSHEET_NS_TRANSITIONAL: &[u8] =
    b"http://schemas.openxmlformats.org/spreadsheetml/2006/main";
pub(super) const SPREADSHEET_NS_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/spreadsheetml/main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SheetVisibility {
    Visible,
    Hidden,
    VeryHidden,
}

impl SheetVisibility {
    pub(super) const fn is_hidden(self) -> bool {
        !matches!(self, Self::Visible)
    }
}

#[derive(Debug, Clone)]
pub(super) struct SheetDescriptor {
    pub(super) name: String,
    pub(super) relationship_id: String,
    pub(super) visibility: SheetVisibility,
}

#[derive(Debug, Clone)]
pub(super) struct DefinedName {
    pub(super) name: String,
    pub(super) definition: String,
    pub(super) local_sheet: Option<usize>,
    pub(super) hidden: bool,
}

#[derive(Debug, Default)]
pub(super) struct WorkbookModel {
    pub(super) sheets: Vec<SheetDescriptor>,
    pub(super) defined_names: Vec<DefinedName>,
}

pub(super) fn parse_workbook(
    bytes: &[u8],
    budget: &mut XmlBudget,
) -> Result<WorkbookModel, ExcelError> {
    #[derive(Debug)]
    struct PendingName {
        name: String,
        definition: String,
        local_sheet: Option<usize>,
        hidden: bool,
    }

    let mut workbook = WorkbookModel::default();
    let mut root_checked = false;
    let mut sheet_name_bytes = 0_usize;
    let mut defined_name_bytes = 0_usize;
    let mut pending_name: Option<PendingName> = None;

    parse_xml_events(bytes, budget, |event| {
        match event {
            XmlEvent::Start(element) | XmlEvent::Empty(element) if !root_checked => {
                root_checked = true;
                require_spreadsheet_element(element, b"workbook")?;
            }
            XmlEvent::Start(element) | XmlEvent::Empty(element)
                if is_spreadsheet_element(element, b"sheet") =>
            {
                if workbook.sheets.len() == MAX_WORKBOOK_SHEETS {
                    return Err(PackageError::PackageLimitExceeded);
                }
                let name = required_attribute(element, b"name")?.to_owned();
                if name.is_empty() {
                    return Err(PackageError::RequiredPartInvalid);
                }
                sheet_name_bytes = sheet_name_bytes
                    .checked_add(name.len())
                    .filter(|bytes| *bytes <= MAX_SHEET_NAME_BYTES)
                    .ok_or(PackageError::PackageLimitExceeded)?;
                let relationship_id = required_attribute(element, b"r:id")?.to_owned();
                let visibility = match optional_attribute(element, b"state") {
                    None | Some("visible") => SheetVisibility::Visible,
                    Some("hidden") => SheetVisibility::Hidden,
                    Some("veryHidden") => SheetVisibility::VeryHidden,
                    Some(_) => return Err(PackageError::RequiredPartInvalid),
                };
                workbook.sheets.push(SheetDescriptor {
                    name,
                    relationship_id,
                    visibility,
                });
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"definedName") => {
                if pending_name.is_some() || workbook.defined_names.len() == MAX_DEFINED_NAMES {
                    return Err(PackageError::PackageLimitExceeded);
                }
                let name = required_attribute(element, b"name")?.to_owned();
                let local_sheet = optional_attribute(element, b"localSheetId")
                    .map(parse_usize)
                    .transpose()?;
                let hidden = optional_attribute(element, b"hidden")
                    .map(parse_bool)
                    .transpose()?
                    .unwrap_or(false);
                pending_name = Some(PendingName {
                    name,
                    definition: String::new(),
                    local_sheet,
                    hidden,
                });
            }
            XmlEvent::Empty(element) if is_spreadsheet_element(element, b"definedName") => {
                if workbook.defined_names.len() == MAX_DEFINED_NAMES {
                    return Err(PackageError::PackageLimitExceeded);
                }
                let name = required_attribute(element, b"name")?.to_owned();
                let local_sheet = optional_attribute(element, b"localSheetId")
                    .map(parse_usize)
                    .transpose()?;
                let hidden = optional_attribute(element, b"hidden")
                    .map(parse_bool)
                    .transpose()?
                    .unwrap_or(false);
                charge_defined_name_bytes(&mut defined_name_bytes, name.len())?;
                workbook.defined_names.push(DefinedName {
                    name,
                    definition: String::new(),
                    local_sheet,
                    hidden,
                });
            }
            XmlEvent::Text(text) => {
                if let Some(name) = &mut pending_name {
                    name.definition.push_str(text);
                }
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"definedName") => {
                let name = pending_name
                    .take()
                    .ok_or(PackageError::RequiredPartInvalid)?;
                charge_defined_name_bytes(
                    &mut defined_name_bytes,
                    name.name.len().saturating_add(name.definition.len()),
                )?;
                workbook.defined_names.push(DefinedName {
                    name: name.name,
                    definition: name.definition,
                    local_sheet: name.local_sheet,
                    hidden: name.hidden,
                });
            }
            _ => {}
        }
        Ok(())
    })
    .map_err(ExcelError::from)?;

    if !root_checked || pending_name.is_some() {
        return Err(ExcelError::RequiredPartInvalid);
    }
    if workbook.defined_names.iter().any(|name| {
        name.local_sheet
            .is_some_and(|index| index >= workbook.sheets.len())
    }) {
        return Err(ExcelError::RequiredPartInvalid);
    }
    Ok(workbook)
}

fn charge_defined_name_bytes(total: &mut usize, bytes: usize) -> Result<(), PackageError> {
    *total = total
        .checked_add(bytes)
        .filter(|total| *total <= MAX_DEFINED_NAME_BYTES)
        .ok_or(PackageError::PackageLimitExceeded)?;
    Ok(())
}

pub(super) fn parse_shared_strings(
    bytes: &[u8],
    budget: &mut XmlBudget,
) -> Result<Vec<String>, ExcelError> {
    let mut strings = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut root_checked = false;
    let mut current: Option<String> = None;
    let mut phonetic_depth = 0_usize;
    let mut capture_text = false;

    parse_xml_events(bytes, budget, |event| {
        match event {
            XmlEvent::Start(element) | XmlEvent::Empty(element) if !root_checked => {
                root_checked = true;
                require_spreadsheet_element(element, b"sst")?;
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"si") => {
                if current.is_some() || strings.len() == MAX_SHARED_STRING_ENTRIES {
                    return Err(PackageError::PackageLimitExceeded);
                }
                current = Some(String::new());
            }
            XmlEvent::Empty(element) if is_spreadsheet_element(element, b"si") => {
                if strings.len() == MAX_SHARED_STRING_ENTRIES {
                    return Err(PackageError::PackageLimitExceeded);
                }
                strings.push(String::new());
            }
            XmlEvent::Start(element)
                if current.is_some() && is_spreadsheet_element(element, b"rPh") =>
            {
                phonetic_depth = phonetic_depth
                    .checked_add(1)
                    .ok_or(PackageError::XmlLimitExceeded)?;
            }
            XmlEvent::Start(element)
                if current.is_some()
                    && phonetic_depth == 0
                    && is_spreadsheet_element(element, b"t") =>
            {
                capture_text = true;
            }
            XmlEvent::Text(text) if capture_text => {
                current
                    .as_mut()
                    .ok_or(PackageError::RequiredPartInvalid)?
                    .push_str(text);
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"t") => {
                capture_text = false;
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"rPh") => {
                phonetic_depth = phonetic_depth
                    .checked_sub(1)
                    .ok_or(PackageError::RequiredPartInvalid)?;
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"si") => {
                let value = current.take().ok_or(PackageError::RequiredPartInvalid)?;
                retained_bytes = retained_bytes
                    .checked_add(value.len())
                    .filter(|bytes| *bytes <= MAX_SHARED_STRING_BYTES)
                    .ok_or(PackageError::PackageLimitExceeded)?;
                strings.push(value);
            }
            _ => {}
        }
        Ok(())
    })
    .map_err(ExcelError::from)?;

    if !root_checked || current.is_some() || phonetic_depth != 0 {
        return Err(ExcelError::RequiredPartInvalid);
    }
    Ok(strings)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CellCoordinate {
    pub(super) row: u32,
    pub(super) column: u32,
}

impl CellCoordinate {
    pub(super) fn a1(self) -> String {
        let mut column = self.column;
        let mut letters = Vec::new();
        while column > 0 {
            let remainder = ((column - 1) % 26) as u8;
            letters.push(char::from(b'A' + remainder));
            column = (column - 1) / 26;
        }
        letters.reverse();
        format!("{}{}", letters.into_iter().collect::<String>(), self.row)
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct CellRecord {
    pub(super) value: Option<String>,
    pub(super) formula: Option<String>,
    pub(super) hidden: bool,
    pub(super) comment: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct WorksheetModel {
    pub(super) cells: BTreeMap<CellCoordinate, CellRecord>,
}

#[derive(Debug)]
struct RowState {
    row: u32,
    hidden: bool,
    last_column: u32,
}

#[derive(Debug)]
struct CellBuilder {
    coordinate: CellCoordinate,
    cell_type: CellType,
    row_hidden: bool,
    value: Option<String>,
    formula: Option<String>,
    inline: String,
    saw_inline: bool,
    field: Option<CellField>,
    phonetic_depth: usize,
    capture_inline_text: bool,
}

#[derive(Debug, Clone, Copy)]
enum CellField {
    Value,
    Formula,
}

#[derive(Debug, Clone, Copy)]
enum CellType {
    Number,
    Shared,
    String,
    Inline,
    Boolean,
    Error,
    Date,
}

pub(super) fn parse_worksheet(
    bytes: &[u8],
    shared_strings: Option<&[String]>,
    budget: &mut XmlBudget,
) -> Result<WorksheetModel, ExcelError> {
    let mut model = WorksheetModel::default();
    let mut root_checked = false;
    let mut last_row = 0_u32;
    let mut row: Option<RowState> = None;
    let mut cell: Option<CellBuilder> = None;
    let mut column_ranges = Vec::<(u32, u32)>::new();
    let mut processed_cells = 0_usize;

    parse_xml_events(bytes, budget, |event| {
        match event {
            XmlEvent::Start(element) | XmlEvent::Empty(element) if !root_checked => {
                root_checked = true;
                require_spreadsheet_element(element, b"worksheet")?;
            }
            XmlEvent::Start(element) | XmlEvent::Empty(element)
                if is_spreadsheet_element(element, b"col") =>
            {
                match optional_attribute(element, b"hidden")
                    .map(parse_bool)
                    .transpose()?
                    .unwrap_or(false)
                {
                    false => {}
                    true => {
                        if column_ranges.len() == MAX_COLUMN_RANGES_PER_SHEET {
                            return Err(PackageError::PackageLimitExceeded);
                        }
                        let min = parse_grid_number(
                            required_attribute(element, b"min")?,
                            MAX_EXCEL_COLUMNS,
                        )?;
                        let max = parse_grid_number(
                            required_attribute(element, b"max")?,
                            MAX_EXCEL_COLUMNS,
                        )?;
                        if min > max {
                            return Err(PackageError::RequiredPartInvalid);
                        }
                        column_ranges.push((min, max));
                    }
                }
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"row") => {
                if row.is_some() || cell.is_some() {
                    return Err(PackageError::RequiredPartInvalid);
                }
                let row_number = optional_attribute(element, b"r")
                    .map(|value| parse_grid_number(value, MAX_EXCEL_ROWS))
                    .transpose()?
                    .unwrap_or_else(|| last_row.saturating_add(1));
                if row_number == 0 || row_number <= last_row {
                    return Err(PackageError::RequiredPartInvalid);
                }
                let hidden = optional_attribute(element, b"hidden")
                    .map(parse_bool)
                    .transpose()?
                    .unwrap_or(false);
                row = Some(RowState {
                    row: row_number,
                    hidden,
                    last_column: 0,
                });
                last_row = row_number;
            }
            XmlEvent::Empty(element) if is_spreadsheet_element(element, b"row") => {
                let row_number = optional_attribute(element, b"r")
                    .map(|value| parse_grid_number(value, MAX_EXCEL_ROWS))
                    .transpose()?
                    .unwrap_or_else(|| last_row.saturating_add(1));
                if row_number == 0 || row_number <= last_row {
                    return Err(PackageError::RequiredPartInvalid);
                }
                last_row = row_number;
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"c") => {
                if cell.is_some() {
                    return Err(PackageError::RequiredPartInvalid);
                }
                let row_state = row.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                processed_cells = processed_cells
                    .checked_add(1)
                    .filter(|count| *count <= MAX_CELL_RECORDS)
                    .ok_or(PackageError::PackageLimitExceeded)?;
                let coordinate = if let Some(reference) = optional_attribute(element, b"r") {
                    let coordinate = parse_a1(reference)?;
                    if coordinate.row != row_state.row || coordinate.column <= row_state.last_column
                    {
                        return Err(PackageError::RequiredPartInvalid);
                    }
                    coordinate
                } else {
                    let column = row_state
                        .last_column
                        .checked_add(1)
                        .filter(|column| *column <= MAX_EXCEL_COLUMNS)
                        .ok_or(PackageError::RequiredPartInvalid)?;
                    CellCoordinate {
                        row: row_state.row,
                        column,
                    }
                };
                row_state.last_column = coordinate.column;
                let cell_type = parse_cell_type(optional_attribute(element, b"t"))?;
                cell = Some(CellBuilder {
                    coordinate,
                    cell_type,
                    row_hidden: row_state.hidden,
                    value: None,
                    formula: None,
                    inline: String::new(),
                    saw_inline: false,
                    field: None,
                    phonetic_depth: 0,
                    capture_inline_text: false,
                });
            }
            XmlEvent::Empty(element) if is_spreadsheet_element(element, b"c") => {
                let row_state = row.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                processed_cells = processed_cells
                    .checked_add(1)
                    .filter(|count| *count <= MAX_CELL_RECORDS)
                    .ok_or(PackageError::PackageLimitExceeded)?;
                let coordinate = if let Some(reference) = optional_attribute(element, b"r") {
                    let coordinate = parse_a1(reference)?;
                    if coordinate.row != row_state.row || coordinate.column <= row_state.last_column
                    {
                        return Err(PackageError::RequiredPartInvalid);
                    }
                    coordinate
                } else {
                    CellCoordinate {
                        row: row_state.row,
                        column: row_state
                            .last_column
                            .checked_add(1)
                            .filter(|column| *column <= MAX_EXCEL_COLUMNS)
                            .ok_or(PackageError::RequiredPartInvalid)?,
                    }
                };
                row_state.last_column = coordinate.column;
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"v") => {
                let builder = cell.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if builder.value.is_some() || builder.field.is_some() {
                    return Err(PackageError::RequiredPartInvalid);
                }
                builder.value = Some(String::new());
                builder.field = Some(CellField::Value);
            }
            XmlEvent::Empty(element) if is_spreadsheet_element(element, b"v") => {
                let builder = cell.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if builder.value.replace(String::new()).is_some() {
                    return Err(PackageError::RequiredPartInvalid);
                }
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"f") => {
                let builder = cell.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if builder.formula.is_some() || builder.field.is_some() {
                    return Err(PackageError::RequiredPartInvalid);
                }
                builder.formula = Some(String::new());
                builder.field = Some(CellField::Formula);
            }
            XmlEvent::Empty(element) if is_spreadsheet_element(element, b"f") => {
                let builder = cell.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if builder.formula.replace(String::new()).is_some() {
                    return Err(PackageError::RequiredPartInvalid);
                }
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"is") => {
                let builder = cell.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if builder.saw_inline {
                    return Err(PackageError::RequiredPartInvalid);
                }
                builder.saw_inline = true;
            }
            XmlEvent::Empty(element) if is_spreadsheet_element(element, b"is") => {
                let builder = cell.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if builder.saw_inline {
                    return Err(PackageError::RequiredPartInvalid);
                }
                builder.saw_inline = true;
            }
            XmlEvent::Start(element)
                if cell.is_some() && is_spreadsheet_element(element, b"rPh") =>
            {
                let builder = cell.as_mut().expect("checked");
                if builder.saw_inline {
                    builder.phonetic_depth = builder
                        .phonetic_depth
                        .checked_add(1)
                        .ok_or(PackageError::XmlLimitExceeded)?;
                }
            }
            XmlEvent::Start(element) if cell.is_some() && is_spreadsheet_element(element, b"t") => {
                let builder = cell.as_mut().expect("checked");
                if builder.saw_inline && builder.phonetic_depth == 0 {
                    builder.capture_inline_text = true;
                }
            }
            XmlEvent::Text(text) => {
                let Some(builder) = cell.as_mut() else {
                    return Ok(());
                };
                if builder.capture_inline_text {
                    builder.inline.push_str(text);
                } else if let Some(field) = builder.field {
                    match field {
                        CellField::Value => builder
                            .value
                            .as_mut()
                            .ok_or(PackageError::RequiredPartInvalid)?
                            .push_str(text),
                        CellField::Formula => builder
                            .formula
                            .as_mut()
                            .ok_or(PackageError::RequiredPartInvalid)?
                            .push_str(text),
                    }
                }
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"v")
                || is_spreadsheet_name(namespace.as_deref(), local_name, b"f") =>
            {
                cell.as_mut()
                    .ok_or(PackageError::RequiredPartInvalid)?
                    .field = None;
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"t") => {
                if let Some(builder) = cell.as_mut() {
                    builder.capture_inline_text = false;
                }
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"rPh") => {
                let builder = cell.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if builder.saw_inline {
                    builder.phonetic_depth = builder
                        .phonetic_depth
                        .checked_sub(1)
                        .ok_or(PackageError::RequiredPartInvalid)?;
                }
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"c") => {
                let builder = cell.take().ok_or(PackageError::RequiredPartInvalid)?;
                if builder.field.is_some() || builder.phonetic_depth != 0 {
                    return Err(PackageError::RequiredPartInvalid);
                }
                let value = resolve_cell_value(&builder, shared_strings)?;
                let formula = builder.formula.filter(|formula| !formula.is_empty());
                if value.is_some() || formula.is_some() {
                    let replaced = model.cells.insert(
                        builder.coordinate,
                        CellRecord {
                            value,
                            formula,
                            hidden: builder.row_hidden,
                            comment: None,
                        },
                    );
                    if replaced.is_some() {
                        return Err(PackageError::RequiredPartInvalid);
                    }
                }
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"row") => {
                match (cell.is_some(), row.take()) {
                    (false, Some(_)) => {}
                    _ => return Err(PackageError::RequiredPartInvalid),
                }
            }
            _ => {}
        }
        Ok(())
    })
    .map_err(ExcelError::from)?;

    if !root_checked || row.is_some() || cell.is_some() {
        return Err(ExcelError::RequiredPartInvalid);
    }
    for (coordinate, record) in &mut model.cells {
        if column_ranges
            .iter()
            .any(|(min, max)| coordinate.column >= *min && coordinate.column <= *max)
        {
            record.hidden = true;
        }
    }
    Ok(model)
}

fn resolve_cell_value(
    builder: &CellBuilder,
    shared_strings: Option<&[String]>,
) -> Result<Option<String>, PackageError> {
    match builder.cell_type {
        CellType::Inline => Ok(builder
            .saw_inline
            .then(|| builder.inline.clone())
            .filter(|v| !v.is_empty())),
        CellType::Shared => {
            let Some(value) = builder.value.as_deref().filter(|value| !value.is_empty()) else {
                return Ok(None);
            };
            let index = parse_usize(value)?;
            let table = shared_strings.ok_or(PackageError::RequiredPartInvalid)?;
            table
                .get(index)
                .cloned()
                .ok_or(PackageError::RequiredPartInvalid)
                .map(|value| (!value.is_empty()).then_some(value))
        }
        CellType::Number
        | CellType::String
        | CellType::Boolean
        | CellType::Error
        | CellType::Date => Ok(builder.value.clone().filter(|value| !value.is_empty())),
    }
}

pub(super) fn parse_comments(
    bytes: &[u8],
    budget: &mut XmlBudget,
) -> Result<BTreeMap<CellCoordinate, String>, ExcelError> {
    #[derive(Debug)]
    struct PendingComment {
        coordinate: CellCoordinate,
        text: String,
        in_text: bool,
        phonetic_depth: usize,
        capture_text: bool,
    }

    let mut comments = BTreeMap::new();
    let mut retained_bytes = 0_usize;
    let mut root_checked = false;
    let mut pending: Option<PendingComment> = None;

    parse_xml_events(bytes, budget, |event| {
        match event {
            XmlEvent::Start(element) | XmlEvent::Empty(element) if !root_checked => {
                root_checked = true;
                require_spreadsheet_element(element, b"comments")?;
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"comment") => {
                if pending.is_some() || comments.len() == MAX_COMMENTS {
                    return Err(PackageError::PackageLimitExceeded);
                }
                pending = Some(PendingComment {
                    coordinate: parse_a1(required_attribute(element, b"ref")?)?,
                    text: String::new(),
                    in_text: false,
                    phonetic_depth: 0,
                    capture_text: false,
                });
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"text") => {
                pending
                    .as_mut()
                    .ok_or(PackageError::RequiredPartInvalid)?
                    .in_text = true;
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"rPh") => {
                let comment = pending.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if comment.in_text {
                    comment.phonetic_depth = comment
                        .phonetic_depth
                        .checked_add(1)
                        .ok_or(PackageError::XmlLimitExceeded)?;
                }
            }
            XmlEvent::Start(element) if is_spreadsheet_element(element, b"t") => {
                let comment = pending.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                if comment.in_text && comment.phonetic_depth == 0 {
                    comment.capture_text = true;
                }
            }
            XmlEvent::Text(text) => {
                if let Some(comment) = &mut pending
                    && comment.capture_text
                {
                    comment.text.push_str(text);
                }
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"t") => {
                pending
                    .as_mut()
                    .ok_or(PackageError::RequiredPartInvalid)?
                    .capture_text = false;
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"rPh") => {
                let comment = pending.as_mut().ok_or(PackageError::RequiredPartInvalid)?;
                comment.phonetic_depth = comment
                    .phonetic_depth
                    .checked_sub(1)
                    .ok_or(PackageError::RequiredPartInvalid)?;
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"text") => {
                pending
                    .as_mut()
                    .ok_or(PackageError::RequiredPartInvalid)?
                    .in_text = false;
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if is_spreadsheet_name(namespace.as_deref(), local_name, b"comment") => {
                let comment = pending.take().ok_or(PackageError::RequiredPartInvalid)?;
                if comment.phonetic_depth != 0 {
                    return Err(PackageError::RequiredPartInvalid);
                }
                retained_bytes = retained_bytes
                    .checked_add(comment.text.len())
                    .filter(|bytes| *bytes <= MAX_COMMENT_BYTES)
                    .ok_or(PackageError::PackageLimitExceeded)?;
                if comments.insert(comment.coordinate, comment.text).is_some() {
                    return Err(PackageError::RequiredPartInvalid);
                }
            }
            _ => {}
        }
        Ok(())
    })
    .map_err(ExcelError::from)?;

    if !root_checked || pending.is_some() {
        return Err(ExcelError::RequiredPartInvalid);
    }
    Ok(comments)
}

pub(super) fn merge_comments(
    worksheet: &mut WorksheetModel,
    comments: BTreeMap<CellCoordinate, String>,
    sheet_hidden: bool,
) {
    for (coordinate, comment) in comments {
        worksheet
            .cells
            .entry(coordinate)
            .or_insert_with(|| CellRecord {
                hidden: sheet_hidden,
                ..CellRecord::default()
            })
            .comment = (!comment.is_empty()).then_some(comment);
    }
}

fn parse_cell_type(value: Option<&str>) -> Result<CellType, PackageError> {
    match value {
        None | Some("n") => Ok(CellType::Number),
        Some("s") => Ok(CellType::Shared),
        Some("str") => Ok(CellType::String),
        Some("inlineStr") => Ok(CellType::Inline),
        Some("b") => Ok(CellType::Boolean),
        Some("e") => Ok(CellType::Error),
        Some("d") => Ok(CellType::Date),
        Some(_) => Err(PackageError::RequiredPartInvalid),
    }
}

pub(super) fn parse_a1(value: &str) -> Result<CellCoordinate, PackageError> {
    let split = value
        .find(|character: char| !character.is_ascii_alphabetic())
        .ok_or(PackageError::RequiredPartInvalid)?;
    let (letters, digits) = value.split_at(split);
    if letters.is_empty()
        || digits.is_empty()
        || !letters.bytes().all(|byte| byte.is_ascii_alphabetic())
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(PackageError::RequiredPartInvalid);
    }
    let column = letters.bytes().try_fold(0_u32, |column, byte| {
        column
            .checked_mul(26)
            .and_then(|column| column.checked_add(u32::from(byte.to_ascii_uppercase() - b'A' + 1)))
    });
    let column = column
        .filter(|column| *column > 0 && *column <= MAX_EXCEL_COLUMNS)
        .ok_or(PackageError::RequiredPartInvalid)?;
    let row = parse_grid_number(digits, MAX_EXCEL_ROWS)?;
    Ok(CellCoordinate { row, column })
}

fn parse_grid_number(value: &str, maximum: u32) -> Result<u32, PackageError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PackageError::RequiredPartInvalid);
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|number| *number > 0 && *number <= maximum)
        .ok_or(PackageError::RequiredPartInvalid)
}

fn parse_usize(value: &str) -> Result<usize, PackageError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PackageError::RequiredPartInvalid);
    }
    value
        .parse::<usize>()
        .map_err(|_| PackageError::RequiredPartInvalid)
}

fn parse_bool(value: &str) -> Result<bool, PackageError> {
    match value {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err(PackageError::RequiredPartInvalid),
    }
}

fn required_attribute<'a>(element: &'a XmlElement, name: &[u8]) -> Result<&'a str, PackageError> {
    optional_attribute(element, name).ok_or(PackageError::RequiredPartInvalid)
}

fn require_spreadsheet_element(
    element: &XmlElement,
    local_name: &[u8],
) -> Result<(), PackageError> {
    if is_spreadsheet_element(element, local_name) {
        Ok(())
    } else {
        Err(PackageError::RequiredPartInvalid)
    }
}

fn is_spreadsheet_element(element: &XmlElement, local_name: &[u8]) -> bool {
    is_spreadsheet_name(
        element.namespace.as_deref(),
        &element.local_name,
        local_name,
    )
}

fn is_spreadsheet_name(namespace: Option<&[u8]>, actual: &[u8], expected: &[u8]) -> bool {
    actual == expected
        && namespace.is_some_and(|namespace| {
            matches!(
                namespace,
                SPREADSHEET_NS_TRANSITIONAL | SPREADSHEET_NS_STRICT
            )
        })
}
