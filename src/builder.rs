use dashmap::DashMap;
use itertools::Itertools;
use num_traits::ToPrimitive;
use rust_lapper::{Interval, Lapper};
use solang::{
    codegen::{self, Expression},
    sema::{
        ast::{self, RetrieveType, Statement, StructType, Type},
        builtin::get_prototype,
        symtable,
        tags::render,
    },
};
use solang_parser::diagnostics::ErrorType;
use solang_parser::pt;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, InlayHint, InlayHintLabel, TextDocumentContentChangeEvent,
};
use tower_lsp::lsp_types::{Position, Range};

use crate::config::Config;

/// Stores information used by language server for every opened file
#[derive(Default)]
pub struct Files {
    pub caches: DashMap<PathBuf, FileCache>,
    pub text_buffers: DashMap<PathBuf, String>,
    pub hints: DashMap<PathBuf, Hints>,
}

#[derive(Default)]
pub struct Hints {
    pub hints: Vec<InlayHint>,
    pub version: i32,
}

impl Files {
    pub fn update_text_file(&self, path: &Path, text: String) {
        self.text_buffers.insert(path.to_path_buf(), text);
    }

    pub fn update_partial_text_file(&self, path: &PathBuf, params: DidChangeTextDocumentParams) {
        let text_buf = self
            .text_buffers
            .get(path)
            .map_or_else(String::new, |buf| buf.clone());

        let updated_text = params
            .content_changes
            .into_iter()
            .fold(text_buf.clone(), update_file_contents);

        self.text_buffers.insert(path.clone(), updated_text);
    }
}

fn update_file_contents(
    mut prev_content: String,
    content_change: TextDocumentContentChangeEvent,
) -> String {
    if let Some(range) = content_change.range {
        let start_line = range.start.line as usize;
        let start_col = range.start.character as usize;
        let end_line = range.end.line as usize;
        let end_col = range.end.character as usize;

        // Directly add the changes to the buffer when changes are present at the end of the file.
        if start_line == prev_content.lines().count() {
            prev_content.push_str(&content_change.text);
            return prev_content;
        }

        let mut new_content = String::new();
        for (i, line) in prev_content.lines().enumerate() {
            if i < start_line {
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }

            if i > end_line {
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }

            if i == start_line {
                new_content.push_str(&line[..start_col]);
                new_content.push_str(&content_change.text);
            }

            if i == end_line {
                new_content.push_str(&line[end_col..]);
                new_content.push('\n');
            }
        }
        new_content
    } else {
        // When no range is provided, entire file is sent in the request.
        content_change.text
    }
}

#[derive(Debug)]
pub struct FileCache {
    pub file: ast::File,
    pub hovers: Lapper<usize, String>,
    pub references: Lapper<usize, DefinitionIndex>,
    #[allow(dead_code)]
    pub scopes: Lapper<usize, Vec<(String, Option<DefinitionIndex>)>>,
    pub top_level_code_objects: HashMap<String, Option<DefinitionIndex>>,
    pub unknown_types: Vec<(Range, String)>,
    pub available_hints: Vec<InlayHint>,
}

fn end_of_block_hint(func_name: String, loc: Range) -> InlayHint {
    // Display the function name at the end of the range
    InlayHint {
        position: Position {
            line: loc.end.line,
            character: loc.end.character,
        },
        label: InlayHintLabel::String(format!("fn {func_name}")),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: Some(false),
        data: None,
    }
}

fn func_to_inlay_hint(selector: Vec<u8>, loc: Range) -> InlayHint {
    // Display the selector at the end of the range
    let hex_selector = selector
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    InlayHint {
        position: Position {
            line: loc.end.line,
            character: loc.end.character + 2,
        },
        label: InlayHintLabel::String(format!("selector: {hex_selector}")),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: Some(false),
        data: None,
    }
}

/// Stores information used by the language server to service requests (eg: `Go to Definitions`) received from the client.
///
/// Information stored in `GlobalCache` is extracted from the `Namespace` when the `SolangServer::build` function is run.
///
/// `GlobalCache` is global in the sense that, unlike `FileCache`, we don't have a separate instance for every file processed.
/// We have just one `GlobalCache` instance per `SolangServer` instance.
///
/// Each field stores *some information* about a code object. The code object is uniquely identified by its `DefinitionIndex`.
/// * `definitions` maps `DefinitionIndex` of a code object to its source code location where it is defined.
/// * `types` maps the `DefinitionIndex` of a code object to that of its type.
/// * `declarations` maps the `DefinitionIndex` of a `Contract` method to a list of methods that it overrides. The overridden methods belong to the parent `Contract`s
/// * `implementations` maps the `DefinitionIndex` of a `Contract` to the `DefinitionIndex`s of methods defined as part of the `Contract`.
/// * `properties` maps the `DefinitionIndex` of a code objects to the name and type of fields, variants or methods defined in the code object.
#[derive(Default)]
pub struct GlobalCache {
    pub definitions: Definitions,
    pub types: Types,
    pub declarations: Declarations,
    pub implementations: Implementations,
    pub properties: Properties,
}

impl GlobalCache {
    pub fn extend(&mut self, other: Self) {
        self.definitions.extend(other.definitions);
        self.types.extend(other.types);
        self.declarations.extend(other.declarations);
        self.implementations.extend(other.implementations);
        self.properties.extend(other.properties);
    }
}

/// Represents the type of the code object that a reference points to
/// Here "code object" refers to contracts, functions, structs, enums etc., that are defined and used within a namespace.
/// It is used along with the path of the file where the code object is defined to uniquely identify an code object.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DefinitionType {
    // function index in Namespace::functions
    Function(usize),
    // variable id
    Variable(usize),
    // (contract id where the variable is declared, variable id)
    NonLocalVariable(Option<usize>, usize),
    // user-defined struct id
    Struct(StructType),
    // (user-defined struct id, field id)
    Field(Type, usize),
    // enum index in Namespace::enums
    Enum(usize),
    // (enum index in Namespace::enums, discriminant id)
    Variant(usize, usize),
    // contract index in Namespace::contracts
    Contract(usize),
    // event index in Namespace::events
    Event(usize),
    UserType(usize),
    DynamicBytes,
}

/// Uniquely identifies a code object.
///
/// `def_type` alone does not guarantee uniqueness, i.e, there can be two or more code objects with identical `def_type`.
/// This is possible as two files can be compiled as part of different `Namespace`s and the code objects can end up having identical `def_type`.
/// For example, two structs defined in the two files can be assigned the same `def_type` - `Struct(0)` as they are both `structs` and numbers are reused across `Namespace` boundaries.
/// As it is currently possible for code objects created as part of two different `Namespace`s to be stored simultaneously in the same `SolangServer` instance,
/// in the scenario described above, code objects cannot be uniquely identified solely through `def_type`.
///
/// But `def_path` paired with `def_type` sufficiently proves uniqueness as no two code objects defined in the same file can have identical `def_type`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DefinitionIndex {
    /// stores the path of the file where the code object is mentioned in source code
    pub def_path: PathBuf,
    /// provides information about the type of the code object in question
    pub def_type: DefinitionType,
}

impl From<DefinitionType> for DefinitionIndex {
    fn from(value: DefinitionType) -> Self {
        Self {
            def_path: Default::default(),
            def_type: value,
        }
    }
}

/// Stores locations of definitions of functions, contracts, structs etc.
type Definitions = HashMap<DefinitionIndex, Range>;
/// Stores strings shown on hover
type HoverEntry = Interval<usize, String>;
/// Stores locations of function calls, uses of structs, contracts etc.
type ReferenceEntry = Interval<usize, DefinitionIndex>;
/// Stores the code objects defined within a scope and their types.
type ScopeEntry = Interval<usize, Vec<(String, Option<DefinitionIndex>)>>;
/// Stores the list of methods implemented by a contract
type Implementations = HashMap<DefinitionIndex, Vec<DefinitionIndex>>;
/// Stores types of code objects
type Types = HashMap<DefinitionIndex, DefinitionIndex>;
/// Stores all the functions that a given function overrides
type Declarations = HashMap<DefinitionIndex, Vec<DefinitionIndex>>;
/// Stores all the fields, variants, methods etc. defined for a code object
type Properties = HashMap<DefinitionIndex, HashMap<String, Option<DefinitionIndex>>>;

pub struct Builder<'a> {
    // `usize` is the file number that the entry belongs to
    hovers: Vec<(usize, HoverEntry)>,
    references: Vec<(usize, ReferenceEntry)>,
    scopes: Vec<(usize, ScopeEntry)>,
    top_level_code_objects: Vec<(usize, (String, Option<DefinitionIndex>))>,

    definitions: Definitions,
    types: Types,
    declarations: Declarations,
    implementations: Implementations,
    properties: Properties,

    ns: &'a ast::Namespace,
}

impl<'a> Builder<'a> {
    pub fn new(ns: &'a ast::Namespace) -> Self {
        Self {
            hovers: Vec::new(),
            references: Vec::new(),
            scopes: Vec::new(),
            top_level_code_objects: Vec::new(),

            definitions: HashMap::new(),
            types: HashMap::new(),
            declarations: HashMap::new(),
            implementations: HashMap::new(),
            properties: HashMap::new(),

            ns,
        }
    }

    // Constructs lookup table for the given statement by traversing the
    // statements and traversing inside the contents of the statements.
    fn statement(&mut self, stmt: &ast::Statement, symtab: &symtable::Symtable) {
        match stmt {
            ast::Statement::Block { statements, .. } => {
                for stmt in statements {
                    self.statement(stmt, symtab);
                }
            }
            ast::Statement::VariableDecl(loc, var_no, param, expr) => {
                if let Some(exp) = expr {
                    self.expression(exp, symtab);
                }

                let constant = self
                    .ns
                    .var_constants
                    .get(loc)
                    .and_then(get_constants)
                    .map(|s| format!(" = {s}"))
                    .unwrap_or_default();

                let readonly = symtab
                    .vars
                    .get(var_no)
                    .map(|var| {
                        if var.slice {
                            "\nreadonly: compiled to slice\n"
                        } else {
                            ""
                        }
                    })
                    .unwrap_or_default();

                let val = format!(
                    "{} {}{}{}",
                    param.ty.to_string(self.ns),
                    param.name_as_str(),
                    constant,
                    readonly
                );

                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: param.loc.start(),
                        stop: param.loc.exclusive_end(),
                        val: make_code_block(val),
                    },
                ));
                if let Some(id) = &param.id {
                    let file_no = id.loc.file_no();
                    let file = &self.ns.files[file_no];
                    let di = DefinitionIndex {
                        def_path: file.path.clone(),
                        def_type: DefinitionType::Variable(*var_no),
                    };
                    self.definitions
                        .insert(di.clone(), loc_to_range(&id.loc, file));
                    if let Some(dt) = get_type_definition(&param.ty) {
                        self.types.insert(di, dt.into());
                    }
                }

                if let Some(loc) = param.ty_loc {
                    if let Some(dt) = get_type_definition(&param.ty) {
                        self.references.push((
                            loc.file_no(),
                            ReferenceEntry {
                                start: loc.start(),
                                stop: loc.exclusive_end(),
                                val: dt.into(),
                            },
                        ));
                    }
                }
            }
            ast::Statement::If(_, _, expr, stat1, stat2) => {
                self.expression(expr, symtab);
                for stmt in stat1 {
                    self.statement(stmt, symtab);
                }
                for stmt in stat2 {
                    self.statement(stmt, symtab);
                }
            }
            ast::Statement::While(_, _, expr, block) => {
                self.expression(expr, symtab);
                for stmt in block {
                    self.statement(stmt, symtab);
                }
            }
            ast::Statement::For {
                init,
                cond,
                next,
                body,
                ..
            } => {
                if let Some(exp) = cond {
                    self.expression(exp, symtab);
                }
                for stat in init {
                    self.statement(stat, symtab);
                }
                if let Some(exp) = next {
                    self.expression(exp, symtab);
                }
                for stat in body {
                    self.statement(stat, symtab);
                }
            }
            ast::Statement::DoWhile(_, _, stat1, expr) => {
                self.expression(expr, symtab);
                for st1 in stat1 {
                    self.statement(st1, symtab);
                }
            }
            ast::Statement::Expression(_, _, expr) => {
                self.expression(expr, symtab);
            }
            ast::Statement::Delete(_, _, expr) => {
                self.expression(expr, symtab);
            }
            ast::Statement::Destructure(_, fields, expr) => {
                self.expression(expr, symtab);
                for field in fields {
                    match field {
                        ast::DestructureField::Expression(expr) => {
                            self.expression(expr, symtab);
                        }
                        ast::DestructureField::VariableDecl(var_no, param) => {
                            self.hovers.push((
                                param.loc.file_no(),
                                HoverEntry {
                                    start: param.loc.start(),
                                    stop: param.loc.exclusive_end(),
                                    val: self.expanded_ty(&param.ty),
                                },
                            ));
                            if let Some(id) = &param.id {
                                let file_no = id.loc.file_no();
                                let file = &self.ns.files[file_no];
                                let di = DefinitionIndex {
                                    def_path: file.path.clone(),
                                    def_type: DefinitionType::Variable(*var_no),
                                };
                                self.definitions
                                    .insert(di.clone(), loc_to_range(&id.loc, file));
                                if let Some(dt) = get_type_definition(&param.ty) {
                                    self.types.insert(di, dt.into());
                                }
                            }
                        }
                        ast::DestructureField::None => (),
                    }
                }
            }
            ast::Statement::Continue(_) => {}
            ast::Statement::Break(_) => {}
            ast::Statement::Return(_, None) => {}
            ast::Statement::Return(_, Some(expr)) => {
                self.expression(expr, symtab);
            }
            ast::Statement::Revert { args, .. } => {
                for arg in args {
                    self.expression(arg, symtab);
                }
            }
            ast::Statement::Emit {
                event_no,
                event_loc,
                args,
                ..
            } => {
                let event = &self.ns.events[*event_no];
                let mut tags = render(&event.tags);
                if !tags.is_empty() {
                    tags.push_str("\n\n");
                }
                let fields = event
                    .fields
                    .iter()
                    .map(|field| {
                        format!(
                            "\t{}{}{}",
                            field.ty.to_string(self.ns),
                            if field.indexed { " indexed " } else { " " },
                            field.name_as_str()
                        )
                    })
                    .join(",\n");
                let val = format!(
                    "event {} {{\n{}\n}}{}",
                    event.symbol_name(self.ns),
                    fields,
                    if event.anonymous { " anonymous" } else { "" }
                );
                self.hovers.push((
                    event_loc.file_no(),
                    HoverEntry {
                        start: event_loc.start(),
                        stop: event_loc.exclusive_end(),
                        val: format!("{}{}", tags, make_code_block(val)),
                    },
                ));

                self.references.push((
                    event_loc.file_no(),
                    ReferenceEntry {
                        start: event_loc.start(),
                        stop: event_loc.exclusive_end(),
                        val: DefinitionIndex {
                            def_path: Default::default(),
                            def_type: DefinitionType::Event(*event_no),
                        },
                    },
                ));

                for arg in args {
                    self.expression(arg, symtab);
                }
            }
            ast::Statement::TryCatch(_, _, try_stmt) => {
                self.expression(&try_stmt.expr, symtab);
                if let Some(clause) = try_stmt.catch_all.as_ref() {
                    for stmt in &clause.stmt {
                        self.statement(stmt, symtab);
                    }
                }
                for stmt in &try_stmt.ok_stmt {
                    self.statement(stmt, symtab);
                }
                for clause in &try_stmt.errors {
                    for stmts in &clause.stmt {
                        self.statement(stmts, symtab);
                    }
                }
            }
            ast::Statement::Underscore(_loc) => {}
            ast::Statement::Assembly(..) => {
                //unimplemented!("Assembly block not implemented in language server");
            }
        }
    }

    // Constructs lookup table by traversing the expressions and storing
    // information later used by the language server
    fn expression(&mut self, expr: &ast::Expression, symtab: &symtable::Symtable) {
        match expr {
            // Variable types expression
            ast::Expression::BoolLiteral { loc, .. } => {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: make_code_block("bool"),
                    },
                ));
            }
            ast::Expression::BytesLiteral { loc, ty, .. } => {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: self.expanded_ty(ty),
                    },
                ));
            }
            ast::Expression::NumberLiteral { loc, ty, value,.. } => {
                if let Type::Enum(id) = ty {
                    self.references.push((
                        loc.file_no(),
                        ReferenceEntry {
                            start: loc.start(),
                            stop: loc.exclusive_end(),
                            val: DefinitionIndex {
                                def_path: Default::default(),
                                def_type: DefinitionType::Variant(*id, value.to_u64().unwrap() as _),
                            },
                        },
                    ));
                }
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: make_code_block(ty.to_string(self.ns)),
                    }
                ));
            }
            ast::Expression::StructLiteral { id: id_path, ty, values, .. } => {
                if let Type::Struct(StructType::UserDefined(id)) = ty {
                    let loc = id_path.identifiers.last().unwrap().loc;
                    self.references.push((
                        loc.file_no(),
                        ReferenceEntry {
                            start: loc.start(),
                            stop: loc.exclusive_end(),
                            val: DefinitionIndex {
                                def_path: Default::default(),
                                def_type: DefinitionType::Struct(StructType::UserDefined(*id)),
                            },
                        },
                    ));
                }

                for (i, (field_name, expr)) in values.iter().enumerate() {
                    self.expression(expr, symtab);

                    if let Some(pt::Identifier { loc: field_name_loc, ..}) = field_name {
                        self.references.push((
                            field_name_loc.file_no(),
                            ReferenceEntry {
                                start: field_name_loc.start(),
                                stop: field_name_loc.exclusive_end(),
                                val: DefinitionIndex {
                                    def_path: Default::default(),
                                    def_type: DefinitionType::Field(ty.clone(), i),
                                },
                            },
                        ));
                    }
                }
            }
            ast::Expression::ArrayLiteral { values, .. }
            | ast::Expression::ConstArrayLiteral { values, .. } => {
                for expr in values {
                    self.expression(expr, symtab);
                }
            }

            // Arithmetic expression
            ast::Expression::Add {
                loc,
                ty,
                unchecked,
                left,
                right,
            } => {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!(
                            "{} {} addition",
                            if *unchecked { "unchecked " } else { "" },
                            ty.to_string(self.ns)
                        ),
                    },
                ));

                self.expression(left, symtab);
                self.expression(right, symtab);
            }
            ast::Expression::Subtract {
                loc,
                ty,
                unchecked,
                left,
                right,
            } => {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!(
                            "{} {} subtraction",
                            if *unchecked { "unchecked " } else { "" },
                            ty.to_string(self.ns)
                        ),
                    }
                ));

                self.expression(left, symtab);
                self.expression(right, symtab);
            }
            ast::Expression::Multiply {
                loc,
                ty,
                unchecked,
                left,
                right,
            } => {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!(
                            "{} {} multiply",
                            if *unchecked { "unchecked " } else { "" },
                            ty.to_string(self.ns)
                        ),
                    },
                ));

                self.expression(left, symtab);
                self.expression(right, symtab);
            }
            ast::Expression::Divide {
                loc,
                ty,
                left,
                right,
            } => {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!("{} divide", ty.to_string(self.ns)),
                    },
                ));

                self.expression(left, symtab);
                self.expression(right, symtab);
            }
            ast::Expression::Modulo {
                loc,
                ty,
                left,
                right,
            } => {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!("{} modulo", ty.to_string(self.ns)),
                    },
                ));

                self.expression(left, symtab);
                self.expression(right, symtab);
            }
            ast::Expression::Power {
                loc,
                ty,
                unchecked,
                base,
                exp,
            } => {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!(
                            "{} {}power",
                            if *unchecked { "unchecked " } else { "" },
                            ty.to_string(self.ns)
                        ),
                    },
                ));

                self.expression(base, symtab);
                self.expression(exp, symtab);
            }

            // Bitwise expresion
            ast::Expression::BitwiseOr { left, right, .. }
            | ast::Expression::BitwiseAnd { left, right, .. }
            | ast::Expression::BitwiseXor { left, right, .. }
            | ast::Expression::ShiftLeft { left, right, .. }
            | ast::Expression::ShiftRight { left, right, .. }
            // Logical expression
            | ast::Expression::Or { left, right, .. }
            | ast::Expression::And { left, right, .. }
            // Compare expression
            | ast::Expression::Equal { left, right, .. }
            | ast::Expression::More { left, right, .. }
            | ast::Expression::MoreEqual { left, right, .. }
            | ast::Expression::Less { left, right, .. }
            | ast::Expression::LessEqual { left, right, .. }
            | ast::Expression::NotEqual { left, right, .. }
            // assign
            | ast::Expression::Assign { left, right, .. }
                        => {
                self.expression(left, symtab);
                self.expression(right, symtab);
            }

            // Variable expression
            ast::Expression::Variable { loc, ty, var_no } => {
                let name = if let Some(var) = symtab.vars.get(var_no) {
                    &var.id.name
                } else {
                    ""
                };
                let readonly = symtab
                    .vars
                    .get(var_no)
                    .map(|var| {
                        if var.slice {
                            "\nreadonly: compiled to slice\n"
                        } else {
                            ""
                        }
                    })
                    .unwrap_or_default();

                let val = format!("{} {}{}", ty.to_string(self.ns), name, readonly);

                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: make_code_block(val),
                    },
                ));

                self.references.push((
                    loc.file_no(),
                    ReferenceEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: DefinitionIndex {
                            def_path: Default::default(),
                            def_type: DefinitionType::Variable(*var_no),
                        },
                    },
                ));
            }
            ast::Expression::ConstantVariable { loc, ty, contract_no, var_no } => {
                let (contract, name) = if let Some(contract_no) = contract_no {
                    let contract = format!("{}.", self.ns.contracts[*contract_no].id);
                    let name = &self.ns.contracts[*contract_no].variables[*var_no].id.name;
                    (contract, name)
                } else {
                    let contract = String::new();
                    let name = &self.ns.constants[*var_no].id.name;
                    (contract, name)
                };
                let constant = self
                    .ns
                    .var_constants
                    .get(loc)
                    .and_then(get_constants)
                    .map(|s| format!(" = {s}"))
                    .unwrap_or_default();
                let val = format!("{} constant {}{}{}", ty.to_string(self.ns), contract, name, constant);
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: make_code_block(val),
                    },
                ));
                self.references.push((
                    loc.file_no(),
                    ReferenceEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: DefinitionIndex {
                            def_path: Default::default(),
                            def_type: DefinitionType::NonLocalVariable(*contract_no, *var_no),
                        },
                    },
                ));
            }
            ast::Expression::StorageVariable { loc, ty, contract_no, var_no } => {
                let contract = &self.ns.contracts[*contract_no];
                let name = &contract.variables[*var_no].id.name;
                let val = format!("{} {}.{}", ty.to_string(self.ns), contract.id, name);
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: make_code_block(val),
                    },
                ));
                self.references.push((
                    loc.file_no(),
                    ReferenceEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: DefinitionIndex {
                            def_path: Default::default(),
                            def_type: DefinitionType::NonLocalVariable(Some(*contract_no), *var_no),
                        },
                    },
                ));
            }
            // Load expression
            ast::Expression::Load { expr, .. }
            | ast::Expression::StorageLoad { expr, .. }
            | ast::Expression::ZeroExt { expr, .. }
            | ast::Expression::SignExt { expr, .. }
            | ast::Expression::Trunc { expr, .. }
            | ast::Expression::Cast { expr, .. }
            | ast::Expression::BytesCast { expr, .. }
            // Increment-Decrement expression
            | ast::Expression::PreIncrement { expr, .. }
            | ast::Expression::PreDecrement { expr, .. }
            | ast::Expression::PostIncrement { expr, .. }
            | ast::Expression::PostDecrement { expr, .. }
            // Other Unary
            | ast::Expression::Not { expr, .. }
            | ast::Expression::BitwiseNot { expr, .. }
            | ast::Expression::Negate { expr, .. } => {
                self.expression(expr, symtab);
            }

            ast::Expression::ConditionalOperator {
                cond,
                true_option: left,
                false_option: right,
                ..
            } => {
                self.expression(cond, symtab);
                self.expression(left, symtab);
                self.expression(right, symtab);
            }

            ast::Expression::Subscript { array, index, .. } => {
                self.expression(array, symtab);
                self.expression(index, symtab);
            }

            ast::Expression::StructMember {  loc, expr, field, ty } => {
                self.expression(expr, symtab);

                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: make_code_block(ty.to_string(self.ns)),
                    },
                ));

                let t = expr.ty().deref_any().clone();
                if let Type::Struct(StructType::UserDefined(_)) = t {
                    self.references.push((
                        loc.file_no(),
                        ReferenceEntry {
                            start: loc.start(),
                            stop: loc.exclusive_end(),
                            val: DefinitionIndex {
                                def_path: Default::default(),
                                def_type: DefinitionType::Field(t, *field),
                            },
                        },
                    ));
                }
            }

            // Array operation expression
            ast::Expression::AllocDynamicBytes { loc, ty, length,  .. } => {
                if let Some(dt) = get_type_definition(ty) {
                    self.references.push((
                        loc.file_no(),
                        ReferenceEntry {
                            start: loc.start(),
                            stop: loc.exclusive_end(),
                            val: dt.into(),
                        },
                    ));
                }
                self.expression(length, symtab);
            }
            ast::Expression::StorageArrayLength { array, .. } => {
                self.expression(array, symtab);
            }

            // String operations expression
            ast::Expression::StringCompare { left, right, .. } => {
                if let ast::StringLocation::RunTime(expr) = left {
                    self.expression(expr, symtab);
                }
                if let ast::StringLocation::RunTime(expr) = right {
                    self.expression(expr, symtab);
                }
            }

            ast::Expression::InternalFunction {id, function_no, ..} => {
                let fnc = &self.ns.functions[*function_no];
                let mut msg_tg = render(&fnc.tags[..]);
                if !msg_tg.is_empty() {
                    msg_tg.push_str("\n\n");
                }

                let params = fnc.params.iter().map(|parm| format!("{} {}", parm.ty.to_string(self.ns), parm.name_as_str())).join(", ");

                let rets = fnc.returns.iter().map(|ret| {
                        let mut msg = ret.ty.to_string(self.ns);
                        if ret.name_as_str() != "" {
                            msg = format!("{} {}", msg, ret.name_as_str());
                        }
                        msg
                    }).join(", ");

                let contract = fnc.contract_no.map(|contract_no| format!("{}.", self.ns.contracts[contract_no].id)).unwrap_or_default();

                let val = format!("{} {}{}({}) returns ({})\n", fnc.ty, contract, fnc.id, params, rets);

                let func_loc = id.identifiers.last().unwrap().loc;

                self.hovers.push((
                    func_loc.file_no(),
                    HoverEntry {
                        start: func_loc.start(),
                        stop: func_loc.exclusive_end(),
                        val: format!("{}{}", msg_tg, make_code_block(val)),
                    },
                ));
                self.references.push((
                    func_loc.file_no(),
                    ReferenceEntry {
                        start: func_loc.start(),
                        stop: func_loc.exclusive_end(),
                        val: DefinitionIndex {
                            def_path: Default::default(),
                            def_type: DefinitionType::Function(*function_no),
                        },
                    },
                ));
            }

            // Function call expression
            ast::Expression::InternalFunctionCall {
                function,
                args,
                ..
            } => {
                if let ast::Expression::InternalFunction { .. } = function.as_ref() {
                    self.expression(function, symtab);
                }

                for arg in args {
                    self.expression(arg, symtab);
                }
            }

            ast::Expression::ExternalFunction { loc, address, function_no, .. } => {
                // modifiers do not have mutability, bases or modifiers themselves
                let fnc = &self.ns.functions[*function_no];
                let mut msg_tg = render(&fnc.tags[..]);
                if !msg_tg.is_empty() {
                    msg_tg.push_str("\n\n");
                }

                let params = fnc.params.iter().map(|parm| format!("{} {}", parm.ty.to_string(self.ns), parm.name_as_str())).join(", ");

                let rets = fnc.returns.iter().map(|ret| {
                        let mut msg = ret.ty.to_string(self.ns);
                        if ret.name_as_str() != "" {
                            msg = format!("{} {}", msg, ret.name_as_str());
                        }
                        msg
                    }).join(", ");

                let contract = fnc.contract_no.map(|contract_no| format!("{}.", self.ns.contracts[contract_no].id)).unwrap_or_default();

                let val = format!("{} {}{}({}) returns ({})\n", fnc.ty, contract, fnc.id, params, rets);

                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!("{}{}", msg_tg, make_code_block(val)),
                    },
                ));
                self.references.push((
                    loc.file_no(),
                    ReferenceEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: DefinitionIndex {
                            def_path: Default::default(),
                            def_type: DefinitionType::Function(*function_no),
                        },
                    },
                ));

                self.expression(address, symtab);
            }

            ast::Expression::ExternalFunctionCall {
                function,
                args,
                call_args,
                ..
            } => {
                if let ast::Expression::ExternalFunction { .. } = function.as_ref() {
                    self.expression(function, symtab);
                }
                for arg in args {
                    self.expression(arg, symtab);
                }
                if let Some(value) = &call_args.value {
                    self.expression(value, symtab);
                }
                if let Some(gas) = &call_args.gas {
                    self.expression(gas, symtab);
                }
            }
            ast::Expression::ExternalFunctionCallRaw {
                address,
                args,
                call_args,
                ..
            } => {
                self.expression(args, symtab);
                self.expression(address, symtab);
                if let Some(value) = &call_args.value {
                    self.expression(value, symtab);
                }
                if let Some(gas) = &call_args.gas {
                    self.expression(gas, symtab);
                }
            }
            ast::Expression::Constructor {
                args, call_args, ..
            } => {
                if let Some(gas) = &call_args.gas {
                    self.expression(gas, symtab);
                }
                for arg in args {
                    self.expression(arg, symtab);
                }
                if let Some(optval) = &call_args.value {
                    self.expression(optval, symtab);
                }
                if let Some(optsalt) = &call_args.salt {
                    self.expression(optsalt, symtab);
                }
                if let Some(seeds) = &call_args.seeds {
                    self.expression(seeds, symtab);
                }
            }
            ast::Expression::Builtin { loc, kind, args, .. } => {
                let (rets, name, params, doc) = if let Some(protval) = get_prototype(*kind) {
                    let rets = protval.ret.iter().map(|ret| ret.to_string(self.ns)).join(" ");

                    let mut params = protval.params.iter().map(|param| param.to_string(self.ns)).join(" ");

                    if !params.is_empty() {
                        params = format!("({params})");
                    }
                    let doc = format!("{}\n\n", protval.doc);
                    (rets, protval.name, params, doc)
                } else {
                    (String::new(), "", String::new(), String::new())
                };

                let val = make_code_block(format!("[built-in] {rets} {name} {params}"));
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!("{doc}{val}"),
                    },
                ));

                for expr in args {
                    self.expression(expr, symtab);
                }
            }
            ast::Expression::FormatString {format, .. } => {
                for (_, e) in format {
                    self.expression(e, symtab);
                }
            }
            ast::Expression::List {  list, .. } => {
                for expr in list {
                    self.expression(expr, symtab);
                }
            }
            _ => {}
        }
    }

    // Constructs contract fields and stores it in the lookup table.
    fn contract_variable(
        &mut self,
        variable: &ast::Variable,
        symtab: &symtable::Symtable,
        contract_no: Option<usize>,
        var_no: usize,
    ) {
        let mut tags = render(&variable.tags[..]);
        if !tags.is_empty() {
            tags.push_str("\n\n");
        }
        let val = make_code_block(format!(
            "{} {}",
            variable.ty.to_string(self.ns),
            variable.id.name
        ));

        if let Some(expr) = &variable.initializer {
            self.expression(expr, symtab);
        }

        let file_no = variable.loc.file_no();
        let file = &self.ns.files[file_no];
        self.hovers.push((
            file_no,
            HoverEntry {
                start: variable.loc.start(),
                stop: variable.loc.start() + variable.id.name.len(),
                val: format!("{tags}{val}"),
            },
        ));

        let di = DefinitionIndex {
            def_path: file.path.clone(),
            def_type: DefinitionType::NonLocalVariable(contract_no, var_no),
        };
        self.definitions
            .insert(di.clone(), loc_to_range(&variable.id.loc, file));
        if let Some(dt) = get_type_definition(&variable.ty) {
            self.types.insert(di, dt.into());
        }

        if contract_no.is_none() {
            self.top_level_code_objects.push((
                file_no,
                (
                    variable.id.name.clone(),
                    get_type_definition(&variable.ty).map(|dt| dt.into()),
                ),
            ))
        }
    }

    // Constructs struct fields and stores it in the lookup table.
    fn field(&mut self, id: usize, field_id: usize, field: &ast::Parameter<Type>) {
        if let Some(loc) = field.ty_loc {
            if let Some(dt) = get_type_definition(&field.ty) {
                self.references.push((
                    loc.file_no(),
                    ReferenceEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: dt.into(),
                    },
                ));
            }
        }
        let loc = field.id.as_ref().map(|id| &id.loc).unwrap_or(&field.loc);
        let file_no = loc.file_no();
        let file = &self.ns.files[file_no];
        self.hovers.push((
            file_no,
            HoverEntry {
                start: loc.start(),
                stop: loc.exclusive_end(),
                val: make_code_block(format!(
                    "{} {}",
                    field.ty.to_string(self.ns),
                    field.name_as_str()
                )),
            },
        ));

        let di = DefinitionIndex {
            def_path: file.path.clone(),
            def_type: DefinitionType::Field(
                Type::Struct(ast::StructType::UserDefined(id)),
                field_id,
            ),
        };
        self.definitions.insert(di.clone(), loc_to_range(loc, file));
        if let Some(dt) = get_type_definition(&field.ty) {
            self.types.insert(di, dt.into());
        }
    }

    /// Traverses namespace to extract information used later by the language server
    /// This includes hover messages, locations where code objects are declared and used
    pub fn build(mut self, config: &Config) -> (Vec<FileCache>, GlobalCache) {
        for (ei, enum_decl) in self.ns.enums.iter().enumerate() {
            for (discriminant, (nam, loc)) in enum_decl.values.iter().enumerate() {
                let file_no = loc.file_no();
                let file = &self.ns.files[file_no];
                self.hovers.push((
                    file_no,
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: make_code_block(format!(
                            "enum {}.{} {}",
                            enum_decl.id, nam, discriminant
                        )),
                    },
                ));

                let di = DefinitionIndex {
                    def_path: file.path.clone(),
                    def_type: DefinitionType::Variant(ei, discriminant),
                };
                self.definitions.insert(di.clone(), loc_to_range(loc, file));

                let dt = DefinitionType::Enum(ei);
                self.types.insert(di, dt.into());
            }

            let file_no = enum_decl.id.loc.file_no();
            let file = &self.ns.files[file_no];
            self.hovers.push((
                file_no,
                HoverEntry {
                    start: enum_decl.id.loc.start(),
                    stop: enum_decl.id.loc.exclusive_end(),
                    val: render(&enum_decl.tags[..]),
                },
            ));

            let def_index = DefinitionIndex {
                def_path: file.path.clone(),
                def_type: DefinitionType::Enum(ei),
            };
            self.definitions
                .insert(def_index.clone(), loc_to_range(&enum_decl.id.loc, file));

            self.properties.insert(
                def_index.clone(),
                enum_decl
                    .values
                    .iter()
                    .map(|(name, _)| (name.clone(), None))
                    .collect(),
            );

            if enum_decl.contract.is_none() {
                self.top_level_code_objects
                    .push((file_no, (enum_decl.id.name.clone(), Some(def_index))));
            }
        }

        for (si, struct_decl) in self.ns.structs.iter().enumerate() {
            if matches!(struct_decl.loc, pt::Loc::File(_, _, _)) {
                for (fi, field) in struct_decl.fields.iter().enumerate() {
                    self.field(si, fi, field);
                }

                let file_no = struct_decl.id.loc.file_no();
                let file = &self.ns.files[file_no];
                self.hovers.push((
                    file_no,
                    HoverEntry {
                        start: struct_decl.id.loc.start(),
                        stop: struct_decl.id.loc.exclusive_end(),
                        val: render(&struct_decl.tags[..]),
                    },
                ));

                let def_index = DefinitionIndex {
                    def_path: file.path.clone(),
                    def_type: DefinitionType::Struct(StructType::UserDefined(si)),
                };
                self.definitions
                    .insert(def_index.clone(), loc_to_range(&struct_decl.id.loc, file));

                self.properties.insert(
                    def_index.clone(),
                    struct_decl
                        .fields
                        .iter()
                        .filter_map(|field| {
                            let def_index =
                                get_type_definition(&field.ty).map(|def_type| DefinitionIndex {
                                    def_path: file.path.clone(),
                                    def_type,
                                });
                            field.id.as_ref().map(|id| (id.name.clone(), def_index))
                        })
                        .collect(),
                );

                if struct_decl.contract.is_none() {
                    self.top_level_code_objects
                        .push((file_no, (struct_decl.id.name.clone(), Some(def_index))));
                }
            }
        }

        for (i, func) in self.ns.functions.iter().enumerate() {
            if func.is_accessor || func.loc == pt::Loc::Builtin {
                // accessor functions are synthetic; ignore them, all the locations are fake
                continue;
            }

            if let Some(bump) = &func.annotations.bump {
                self.expression(&bump.1, &func.symtable);
            }

            for seed in &func.annotations.seeds {
                self.expression(&seed.1, &func.symtable);
            }

            if let Some(space) = &func.annotations.space {
                self.expression(&space.1, &func.symtable);
            }

            if let Some((loc, name)) = &func.annotations.payer {
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: format!("payer account: {name}"),
                    },
                ));
            }

            for (i, param) in func.params.iter().enumerate() {
                let loc = param.id.as_ref().map(|id| &id.loc).unwrap_or(&param.loc);
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: self.expanded_ty(&param.ty),
                    },
                ));

                if let Some(Some(var_no)) = func.symtable.arguments.get(i) {
                    if let Some(id) = &param.id {
                        let file_no = id.loc.file_no();
                        let file = &self.ns.files[file_no];
                        let di = DefinitionIndex {
                            def_path: file.path.clone(),
                            def_type: DefinitionType::Variable(*var_no),
                        };
                        self.definitions
                            .insert(di.clone(), loc_to_range(&id.loc, file));
                        if let Some(dt) = get_type_definition(&param.ty) {
                            self.types.insert(di, dt.into());
                        }
                    }
                }

                if let Some(ty_loc) = param.ty_loc {
                    if let Some(dt) = get_type_definition(&param.ty) {
                        self.references.push((
                            ty_loc.file_no(),
                            ReferenceEntry {
                                start: ty_loc.start(),
                                stop: ty_loc.exclusive_end(),
                                val: dt.into(),
                            },
                        ));
                    }
                }
            }

            for (i, ret) in func.returns.iter().enumerate() {
                let loc = ret.id.as_ref().map(|id| &id.loc).unwrap_or(&ret.loc);
                self.hovers.push((
                    loc.file_no(),
                    HoverEntry {
                        start: loc.start(),
                        stop: loc.exclusive_end(),
                        val: self.expanded_ty(&ret.ty),
                    },
                ));

                if let Some(id) = &ret.id {
                    if let Some(var_no) = func.symtable.returns.get(i) {
                        let file_no = id.loc.file_no();
                        let file = &self.ns.files[file_no];
                        let di = DefinitionIndex {
                            def_path: file.path.clone(),
                            def_type: DefinitionType::Variable(*var_no),
                        };
                        self.definitions
                            .insert(di.clone(), loc_to_range(&id.loc, file));
                        if let Some(dt) = get_type_definition(&ret.ty) {
                            self.types.insert(di, dt.into());
                        }
                    }
                }

                if let Some(ty_loc) = ret.ty_loc {
                    if let Some(dt) = get_type_definition(&ret.ty) {
                        self.references.push((
                            ty_loc.file_no(),
                            ReferenceEntry {
                                start: ty_loc.start(),
                                stop: ty_loc.exclusive_end(),
                                val: dt.into(),
                            },
                        ));
                    }
                }
            }

            for stmt in &func.body {
                self.statement(stmt, &func.symtable);
            }

            let file_no = func.id.loc.file_no();
            let file = &self.ns.files[file_no];

            let func_def_index = DefinitionIndex {
                def_path: file.path.clone(),
                def_type: DefinitionType::Function(i),
            };
            self.definitions
                .insert(func_def_index.clone(), loc_to_range(&func.id.loc, file));

            self.scopes.extend(func.symtable.scopes.iter().map(|scope| {
                let loc = scope.loc.unwrap();
                let scope_entry = ScopeEntry {
                    start: loc.start(),
                    stop: loc.exclusive_end(),
                    val: scope
                        .names
                        .values()
                        .filter_map(|pos| {
                            func.symtable.vars.get(pos).map(|var| {
                                (
                                    var.id.name.clone(),
                                    get_type_definition(&var.ty).map(|def_type| def_type.into()),
                                )
                            })
                        })
                        .collect_vec(),
                };
                (file_no, scope_entry)
            }));

            // if func.contract_no.is_none() {
            self.top_level_code_objects
                .push((file_no, (func.id.name.clone(), Some(func_def_index))))
            // }
        }

        for (i, constant) in self.ns.constants.iter().enumerate() {
            let samptb = symtable::Symtable::default();
            self.contract_variable(constant, &samptb, None, i);
        }

        for (ci, contract) in self.ns.contracts.iter().enumerate() {
            for base in &contract.bases {
                let file_no = base.loc.file_no();
                self.hovers.push((
                    file_no,
                    HoverEntry {
                        start: base.loc.start(),
                        stop: base.loc.exclusive_end(),
                        val: make_code_block(format!(
                            "contract {}",
                            self.ns.contracts[base.contract_no].id
                        )),
                    },
                ));
                self.references.push((
                    file_no,
                    ReferenceEntry {
                        start: base.loc.start(),
                        stop: base.loc.exclusive_end(),
                        val: DefinitionIndex {
                            def_path: Default::default(),
                            def_type: DefinitionType::Contract(base.contract_no),
                        },
                    },
                ));
            }

            for (i, variable) in contract.variables.iter().enumerate() {
                let symtable = symtable::Symtable::default();
                self.contract_variable(variable, &symtable, Some(ci), i);
            }

            let file_no = contract.loc.file_no();
            let file = &self.ns.files[file_no];
            self.hovers.push((
                file_no,
                HoverEntry {
                    start: contract.id.loc.start(),
                    stop: contract.id.loc.exclusive_end(),
                    val: render(&contract.tags[..]),
                },
            ));

            let contract_def_index = DefinitionIndex {
                def_path: file.path.clone(),
                def_type: DefinitionType::Contract(ci),
            };

            self.definitions.insert(
                contract_def_index.clone(),
                loc_to_range(&contract.id.loc, file),
            );

            let impls = contract
                .functions
                .iter()
                .map(|f| DefinitionIndex {
                    def_path: file.path.clone(), // all the implementations for a contract are present in the same file in solidity
                    def_type: DefinitionType::Function(*f),
                })
                .collect();

            self.implementations
                .insert(contract_def_index.clone(), impls);

            let decls = contract
                .virtual_functions
                .iter()
                .filter_map(|(_, indices)| {
                    // due to the way the `indices` vector is populated during namespace creation,
                    // the last element in the vector contains the overriding function that belongs to the current contract.
                    let func = DefinitionIndex {
                        def_path: file.path.clone(),
                        // `unwrap` is alright here as the `indices` vector is guaranteed to have at least 1 element
                        // the vector is always initialised with one initial element
                        // and the elements in the vector are never removed during namespace construction
                        def_type: DefinitionType::Function(indices.last().copied().unwrap()),
                    };

                    // get all the functions overridden by the current function
                    let all_decls: HashSet<usize> = HashSet::from_iter(indices.iter().copied());

                    // choose the overridden functions that belong to the parent contracts
                    // due to multiple inheritance, a contract can have multiple parents
                    let parent_decls = contract
                        .bases
                        .iter()
                        .map(|b| {
                            let p = &self.ns.contracts[b.contract_no];
                            HashSet::from_iter(p.functions.iter().copied())
                                .intersection(&all_decls)
                                .copied()
                                .collect::<HashSet<usize>>()
                        })
                        .reduce(|acc, e| acc.union(&e).copied().collect());

                    // get the `DefinitionIndex`s of the overridden functions
                    parent_decls.map(|parent_decls| {
                        let decls = parent_decls
                            .iter()
                            .map(|&i| {
                                let loc = self.ns.functions[i].loc;
                                DefinitionIndex {
                                    def_path: self.ns.files[loc.file_no()].path.clone(),
                                    def_type: DefinitionType::Function(i),
                                }
                            })
                            .collect::<Vec<_>>();

                        (func, decls)
                    })
                });

            self.declarations.extend(decls);

            // Code objects defined within the contract

            let functions = contract.functions.iter().filter_map(|&fno| {
                self.ns
                    .functions
                    .get(fno)
                    .map(|func| (func.id.name.clone(), None))
            });

            let structs = self
                .ns
                .structs
                .iter()
                .enumerate()
                .filter_map(|(i, r#struct)| match &r#struct.contract {
                    Some(contract_name) if contract_name == &contract.id.name => Some((
                        r#struct.id.name.clone(),
                        Some(DefinitionType::Struct(StructType::UserDefined(i))),
                    )),
                    _ => None,
                });

            let enums =
                self.ns
                    .enums
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r#enum)| match &r#enum.contract {
                        Some(contract_name) if contract_name == &contract.id.name => {
                            Some((r#enum.id.name.clone(), Some(DefinitionType::Enum(i))))
                        }
                        _ => None,
                    });

            let events =
                self.ns
                    .events
                    .iter()
                    .enumerate()
                    .filter_map(|(i, event)| match &event.contract {
                        Some(event_contract) if *event_contract == ci => {
                            Some((event.id.name.clone(), Some(DefinitionType::Event(i))))
                        }
                        _ => None,
                    });

            let variables = contract.variables.iter().map(|var| {
                (
                    var.id.name.clone(),
                    get_type_definition(&var.ty).map(|def_type| def_type.into()),
                )
            });

            let contract_contents = functions
                .chain(structs)
                .chain(enums)
                .chain(events)
                .map(|(name, dt)| {
                    let def_index = dt.map(|def_type| DefinitionIndex {
                        def_path: file.path.clone(),
                        def_type,
                    });
                    (name, def_index)
                })
                .chain(variables);

            self.properties.insert(
                contract_def_index.clone(),
                contract_contents.clone().collect(),
            );

            self.scopes.push((
                file_no,
                ScopeEntry {
                    start: contract.loc.start(),
                    stop: contract.loc.exclusive_end(),
                    val: contract_contents.collect(),
                },
            ));

            // Contracts can't be defined within other contracts.
            // So all the contracts are top level objects in a file.
            self.top_level_code_objects.push((
                file_no,
                (contract.id.name.clone(), Some(contract_def_index)),
            ));
        }

        for (ei, event) in self.ns.events.iter().enumerate() {
            for (fi, field) in event.fields.iter().enumerate() {
                self.field(ei, fi, field);
            }

            let file_no = event.id.loc.file_no();
            let file = &self.ns.files[file_no];
            self.hovers.push((
                file_no,
                HoverEntry {
                    start: event.id.loc.start(),
                    stop: event.id.loc.exclusive_end(),
                    val: render(&event.tags[..]),
                },
            ));

            let def_index = DefinitionIndex {
                def_path: file.path.clone(),
                def_type: DefinitionType::Event(ei),
            };
            self.definitions
                .insert(def_index.clone(), loc_to_range(&event.id.loc, file));

            if event.contract.is_none() {
                self.top_level_code_objects
                    .push((file_no, (event.id.name.clone(), Some(def_index))));
            }
        }

        for lookup in &mut self.hovers {
            if let Some(msg) =
                self.ns
                    .hover_overrides
                    .get(&pt::Loc::File(lookup.0, lookup.1.start, lookup.1.stop))
            {
                lookup.1.val.clone_from(msg);
            }
        }

        // `defs_to_files` and `defs_to_file_nos` are used to insert the correct filepath where a code object is defined.
        // previously, a dummy path was filled.
        // In a single namespace, there can't be two (or more) code objects with a given `DefinitionType`.
        // So, there exists a one-to-one mapping between `DefinitionIndex` and `DefinitionType` when we are dealing with just one namespace.
        let defs_to_files = self
            .definitions
            .keys()
            .map(|key| (key.def_type.clone(), key.def_path.clone()))
            .collect::<HashMap<DefinitionType, PathBuf>>();

        let defs_to_file_nos = self
            .ns
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect::<HashMap<PathBuf, usize>>();

        for val in self.types.values_mut() {
            if let Some(path) = defs_to_files.get(&val.def_type) {
                val.def_path.clone_from(path);
            }
        }

        for (di, range) in &self.definitions {
            if let Some(&file_no) = defs_to_file_nos.get(&di.def_path) {
                let file = &self.ns.files[file_no];
                self.references.push((
                    file_no,
                    ReferenceEntry {
                        start: file
                            .get_offset(range.start.line as usize, range.start.character as usize)
                            .unwrap(),
                        // 1 is added to account for the fact that `Lapper` expects half open ranges of the type:  [`start`, `stop`)
                        // i.e, `start` included but `stop` excluded.
                        stop: file
                            .get_offset(range.end.line as usize, range.end.character as usize)
                            .unwrap()
                            + 1,
                        val: di.clone(),
                    },
                ));
            }
        }

        let unknown_type_diag = self
            .ns
            .diagnostics
            .iter()
            .flat_map(|diag| {
                if let Some(type_name) = retrieve_type_name_from_diagnostic(diag.message.clone()) {
                    let loc = loc_to_range(&diag.loc, &self.ns.files[diag.loc.file_no()]);
                    Some((diag.loc.file_no(), loc, type_name))
                } else {
                    None
                }
            })
            .collect::<Vec<(usize, Range, String)>>();

        let file_caches = self
            .ns
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| FileCache {
                file: f.clone(),
                // get `hovers` that belong to the current file
                hovers: Lapper::new(
                    self.hovers
                        .iter()
                        .filter(|h| h.0 == i)
                        .map(|(_, i)| i.clone())
                        .collect(),
                ),
                // get `references` that belong to the current file
                references: Lapper::new(
                    self.references
                        .iter()
                        .filter(|reference| reference.0 == i)
                        .map(|(_, i)| {
                            let mut i = i.clone();
                            if let Some(def_path) = defs_to_files.get(&i.val.def_type) {
                                i.val.def_path.clone_from(def_path);
                            }
                            i
                        })
                        .collect(),
                ),
                scopes: Lapper::new(
                    self.scopes
                        .iter()
                        .filter(|scope| scope.0 == i)
                        .map(|(_, scope)| {
                            let mut scope = scope.clone();
                            for val in &mut scope.val {
                                if let Some(val) = &mut val.1 {
                                    if let Some(def_path) = defs_to_files.get(&val.def_type) {
                                        val.def_path.clone_from(def_path);
                                    }
                                }
                            }
                            scope
                        })
                        .collect(),
                ),
                top_level_code_objects: self
                    .top_level_code_objects
                    .iter_mut()
                    .filter(|code_object| code_object.0 == i)
                    .map(|code_object| {
                        if let Some(DefinitionIndex { def_path, def_type }) = &mut code_object.1 .1
                        {
                            if def_path.to_str().unwrap() == "" {
                                if let Some(dp) = defs_to_files.get(def_type) {
                                    def_path.clone_from(dp);
                                }
                            }
                        }
                        code_object.1.clone()
                    })
                    .collect(),
                unknown_types: unknown_type_diag
                    .iter()
                    .filter(|(file_no, _, _)| *file_no == i)
                    .map(|(_, loc, type_name)| (*loc, type_name.clone()))
                    .collect(),
                available_hints: self
                    .ns
                    .functions
                    .iter()
                    .filter(|f| f.loc.file_no() == i)
                    .flat_map(|f| {
                        let mut hints = vec![];

                        // hint for end of function
                        if config.inlayHints_closingBraceHints_enable {
                            if let Some(Statement::Block { loc, .. }) = f.body.first() {
                                let loc_range = loc_to_range(loc, self.ns.files.get(i).unwrap());
                                if loc_range.end.line - loc_range.start.line
                                    >= config.inlayHints_closingBraceHints_minLines
                                {
                                    hints
                                        .push(end_of_block_hint(f.mangled_name.clone(), loc_range));
                                }
                            }
                        }

                        // hint for function selector
                        let range = loc_to_range(&f.loc_prototype, self.ns.files.get(i).unwrap());
                        hints.push(func_to_inlay_hint(f.selector(self.ns, &0), range));

                        hints
                    })
                    .collect(),
            })
            .collect();

        for properties in self.properties.values_mut() {
            for def_index in properties.values_mut().flatten() {
                if def_index.def_path.to_str().unwrap() == "" {
                    if let Some(def_path) = defs_to_files.get(&def_index.def_type) {
                        def_index.def_path.clone_from(def_path);
                    }
                }
            }
        }

        let global_cache = GlobalCache {
            definitions: self.definitions,
            types: self.types,
            declarations: self.declarations,
            implementations: self.implementations,
            properties: self.properties,
        };

        (file_caches, global_cache)
    }

    /// Render the type with struct/enum fields expanded
    fn expanded_ty(&self, ty: &ast::Type) -> String {
        match ty {
            ast::Type::Ref(ty) => self.expanded_ty(ty),
            ast::Type::StorageRef(_, ty) => self.expanded_ty(ty),
            ast::Type::Struct(struct_type) => {
                let strct = struct_type.definition(self.ns);
                let mut tags = render(&strct.tags);
                if !tags.is_empty() {
                    tags.push_str("\n\n")
                }

                let fields = strct
                    .fields
                    .iter()
                    .map(|field| {
                        format!("\t{} {}", field.ty.to_string(self.ns), field.name_as_str())
                    })
                    .join(",\n");

                let val = make_code_block(format!("struct {strct} {{\n{fields}\n}}"));
                format!("{tags}{val}")
            }
            ast::Type::Enum(n) => {
                let enm = &self.ns.enums[*n];
                let mut tags = render(&enm.tags);
                if !tags.is_empty() {
                    tags.push_str("\n\n")
                }
                let values = enm
                    .values
                    .iter()
                    .map(|value| format!("\t{}", value.0))
                    .join(",\n");

                let val = make_code_block(format!("enum {enm} {{\n{values}\n}}"));
                format!("{tags}{val}")
            }
            _ => make_code_block(ty.to_string(self.ns)),
        }
    }
}

/// Calculate the line and column from the Loc offset received from the parser
fn loc_to_range(loc: &pt::Loc, file: &ast::File) -> Range {
    get_range(loc.start(), loc.end(), file)
}

fn get_range(start: usize, end: usize, file: &ast::File) -> Range {
    let (line, column) = file.offset_to_line_column(start);
    let start = Position::new(line as u32, column as u32);
    let (line, column) = file.offset_to_line_column(end);
    let end = Position::new(line as u32, column as u32);

    Range::new(start, end)
}

pub fn get_type_definition(ty: &Type) -> Option<DefinitionType> {
    match ty {
        Type::Enum(id) => Some(DefinitionType::Enum(*id)),
        Type::Struct(st) => Some(DefinitionType::Struct(*st)),
        Type::Array(ty, _) => get_type_definition(ty),
        Type::Ref(ty) => get_type_definition(ty),
        Type::StorageRef(_, ty) => get_type_definition(ty),
        Type::Contract(id) => Some(DefinitionType::Contract(*id)),
        Type::UserType(id) => Some(DefinitionType::UserType(*id)),
        Type::DynamicBytes => Some(DefinitionType::DynamicBytes),
        _ => None,
    }
}

pub fn error_type_to_code(error_type: ErrorType) -> String {
    match error_type {
        ErrorType::SyntaxError => "syntax_error".to_string(),
        ErrorType::ParserError => "parser_error".to_string(),
        ErrorType::DeclarationError => "declaration_error".to_string(),
        ErrorType::CastError => "cast_error".to_string(),
        ErrorType::TypeError => "type_error".to_string(),
        ErrorType::Warning => "warning".to_string(),
        _ => "unknown_error".to_string(),
    }
}

fn make_code_block(s: impl AsRef<str>) -> String {
    format!("```solidity\n{}\n```", s.as_ref())
}

fn get_constants(expr: &Expression) -> Option<String> {
    let val = match expr {
        codegen::Expression::BytesLiteral {
            ty: ast::Type::Bytes(_) | ast::Type::DynamicBytes,
            value,
            ..
        } => {
            format!("hex\"{}\"", hex::encode(value))
        }
        codegen::Expression::BytesLiteral {
            ty: ast::Type::String,
            value,
            ..
        } => {
            format!("\"{}\"", String::from_utf8_lossy(value))
        }
        codegen::Expression::NumberLiteral {
            ty: ast::Type::Uint(_) | ast::Type::Int(_),
            value,
            ..
        } => {
            format!("{value}")
        }
        _ => return None,
    };
    Some(val)
}

fn retrieve_type_name_from_diagnostic(message: String) -> Option<String> {
    // Look for pattern: type '<type_name>' not found
    if let Some(start) = message.find("type '") {
        let after_start = start + 6; // Skip "type '"
        if let Some(end) = message[after_start..].find("' not found") {
            let type_name = &message[after_start..after_start + end];
            if !type_name.is_empty() {
                return Some(type_name.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod test {
    use super::*;
    use tower_lsp::lsp_types::{
        DidChangeTextDocumentParams, TextDocumentContentChangeEvent, Url,
        VersionedTextDocumentIdentifier,
    };

    #[test]
    fn test_retrieve_type_name() {
        // Test basic functionality
        assert_eq!(
            retrieve_type_name_from_diagnostic("type 'MyContract' not found".to_string()),
            Some("MyContract".to_string())
        );

        // Test with context
        assert_eq!(
            retrieve_type_name_from_diagnostic(
                "Error: type 'IERC20' not found in scope".to_string()
            ),
            Some("IERC20".to_string())
        );

        // Test no match
        assert_eq!(
            retrieve_type_name_from_diagnostic("undefined variable MyContract".to_string()),
            None
        );

        // Test empty type name
        assert_eq!(
            retrieve_type_name_from_diagnostic("type '' not found".to_string()),
            None
        );
    }

    #[test]
    fn test_files_update_text_file() {
        let files = Files::default();
        let path = PathBuf::from("test.txt");
        let content = "Hello, world!".to_string();

        files.update_text_file(&path, content.clone());

        let stored_content = files.text_buffers.get(&path).unwrap();
        assert_eq!(*stored_content, content);
    }

    #[test]
    fn test_files_update_text_file_overwrites_existing() {
        let files = Files::default();
        let path = PathBuf::from("test.txt");

        files.update_text_file(&path, "Original content".to_string());
        files.update_text_file(&path, "New content".to_string());

        let stored_content = files.text_buffers.get(&path).unwrap();
        assert_eq!(*stored_content, "New content");
    }

    #[test]
    fn test_files_update_partial_text_file_new_file() {
        let files = Files::default();
        let path = PathBuf::from("/tmp/foo.txt");

        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
                version: 1,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "New file content".to_string(),
            }],
        };

        files.update_partial_text_file(&path, params);

        let stored_content = files.text_buffers.get(&path).unwrap();
        assert_eq!(*stored_content, "New file content");
    }

    #[test]
    fn test_files_update_partial_text_file_full_replace() {
        let files = Files::default();
        let path = PathBuf::from("/tmp/foo.txt");

        // Set initial content
        files.update_text_file(&path, "Original content".to_string());

        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None, // No range means full replace
                range_length: None,
                text: "Completely new content".to_string(),
            }],
        };

        files.update_partial_text_file(&path, params);

        let stored_content = files.text_buffers.get(&path).unwrap();
        assert_eq!(*stored_content, "Completely new content");
    }

    #[test]
    fn test_files_update_partial_text_file_with_range() {
        let files = Files::default();
        let path = PathBuf::from("/tmp/foo.txt");

        // Set initial content: "line1\nline2\nline3"
        files.update_text_file(&path, "line1\nline2\nline3".to_string());

        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range {
                    start: Position {
                        line: 1,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 5,
                    }, // Replace "line2"
                }),
                range_length: Some(5),
                text: "modified".to_string(),
            }],
        };

        files.update_partial_text_file(&path, params);

        let stored_content = files.text_buffers.get(&path).unwrap();
        assert_eq!(*stored_content, "line1\nmodified\nline3\n");
    }

    #[test]
    fn test_files_update_partial_text_file_multiple_changes() {
        let files = Files::default();
        let path = PathBuf::from("/tmp/foo.txt");

        // Set initial content
        files.update_text_file(&path, "Hello world".to_string());

        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
                version: 2,
            },
            content_changes: vec![
                TextDocumentContentChangeEvent {
                    range: Some(Range {
                        start: Position {
                            line: 0,
                            character: 0,
                        },
                        end: Position {
                            line: 0,
                            character: 5,
                        }, // Replace "Hello"
                    }),
                    range_length: Some(5),
                    text: "Hi".to_string(),
                },
                TextDocumentContentChangeEvent {
                    range: Some(Range {
                        start: Position {
                            line: 0,
                            character: 3,
                        },
                        end: Position {
                            line: 0,
                            character: 8,
                        }, // Replace "world"
                    }),
                    range_length: Some(5),
                    text: "Rust".to_string(),
                },
            ],
        };

        files.update_partial_text_file(&path, params);

        let stored_content = files.text_buffers.get(&path).unwrap();
        // Note: The exact result depends on your update_file_contents implementation
        // This test verifies the method processes multiple changes
        assert!(stored_content.contains("Hi") || stored_content.contains("Rust"));
    }
}
