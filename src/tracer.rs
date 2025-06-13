use alloy_primitives::Address;
use alloy_primitives::Bytes;
use foundry_compilers::artifacts::ast::{self, Node, NodeType};
use foundry_compilers::artifacts::sourcemap::Jump;
use foundry_compilers::artifacts::sourcemap::SourceElement;
use foundry_compilers::artifacts::sourcemap::{parse, SourceMap};
use foundry_compilers::artifacts::CompactBytecode;
use foundry_compilers::artifacts::ConfigurableContractArtifact;
use foundry_compilers::resolver::parse::SolData;
use foundry_compilers::ProjectPathsConfig;
use revm_inspectors::tracing::types::CallTraceStep;
use serde::Deserialize;
use serde::Serialize;
use slice_group_by::GroupBy;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::fs;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Deserialize)]
struct DebuggerContext {
    pub debug_arena: Vec<DebugNode>,
    pub contracts: ContractsDump,
}

#[derive(Deserialize)]
pub enum DebugNodeKind {
    #[serde(rename = "CALL")]
    Call,
    #[serde(rename = "CREATE")]
    Create,
    #[serde(rename = "STATICCALL")]
    StaticCall,
}

impl DebugNodeKind {
    fn is_create(&self) -> bool {
        matches!(self, DebugNodeKind::Create)
    }
}

#[derive(Deserialize)]
pub struct DebugNode {
    pub kind: DebugNodeKind,
    pub address: Address,
    pub steps: Vec<CallTraceStep>,
}

#[derive(Deserialize)]
pub struct ContractsDump {
    pub identified_contracts: HashMap<Address, String>,
}

fn generate_debug_units(
    root_path: &Path,
    contracts_involved: Option<&HashSet<String>>,
) -> Result<HashMap<String, DebugUnit>, Box<dyn std::error::Error>> {
    let config: ProjectPathsConfig<SolData> =
        ProjectPathsConfig::dapptools(Path::new(root_path)).unwrap();
    let artifacts = load_artifacts(&config.artifacts).unwrap();

    let mut debug_unit = HashMap::new();

    for (file_path, artifact) in artifacts {
        if let Some(ast) = artifact.ast.clone() {
            let absolute_path = ast.absolute_path;

            // check if the absolute_path starts with 'lib/'
            // for now skip all the libs since we cannot parse them yet
            if absolute_path.starts_with("lib/") {
                continue;
            }

            // Extract the compilation target contract name from the artifact path.
            // Each JSON artifact represents one compilation target, but the AST includes
            // all contracts/nodes from the source file plus imported dependencies.
            // The filename tells us which specific contract was the compilation target.
            // IMPORTANT: Bytecode is only available for the compilation target contract.
            // Attempting to decode other ContractDefinitions with a visitor will fail.
            // e.g., "Parent.json" = Parent is the target with bytecode, but AST contains Parent + Child + imports
            let name = file_path.file_name().unwrap().to_str().unwrap();
            let name = name
                .split('/')
                .next_back()
                .unwrap_or("")
                .strip_suffix(".json")
                .unwrap_or("");

            if let Some(contracts_involved) = contracts_involved {
                if !contracts_involved.contains(name) {
                    continue;
                }
            }

            ast.nodes.iter().for_each(|node| {
                let node = node.clone();

                if let Some(deployed_bytecode) = artifact.deployed_bytecode.as_ref() {
                    let deployed_bytecode = deployed_bytecode.bytecode.as_ref().unwrap().clone();
                    let bytecode = artifact.bytecode.as_ref().unwrap().clone();

                    if node.node_type == NodeType::ContractDefinition {
                        let contract_name = node.attribute::<String>("name").unwrap();

                        if contract_name == name {
                            let source =
                                fs::read_to_string(root_path.join(absolute_path.clone())).unwrap();

                            let mut visitor = StatementVisitor::new(
                                deployed_bytecode,
                                bytecode,
                                root_path
                                    .join(absolute_path.clone())
                                    .to_string_lossy()
                                    .to_string(),
                                source,
                            );
                            visitor.visit_contract(&node.clone()).unwrap();

                            // just so that we can keep the reference around
                            let mut dd = visitor.debug_unit;
                            dd.source_id = artifact.id.unwrap();

                            debug_unit.insert(contract_name, dd);
                        }
                    }
                }
            });
        } else {
            println!("ast not found")
        }
    }

    Ok(debug_unit)
}

fn load_artifacts(
    artifacts_path: &Path,
) -> Result<Vec<(PathBuf, ConfigurableContractArtifact)>, Box<dyn std::error::Error>> {
    let mut artifacts = Vec::new();

    // Read all directories in the artifacts path
    for entry in fs::read_dir(artifacts_path)? {
        let entry = entry?;
        let path = entry.path();

        // if path is build-info, skip
        if path.file_name().unwrap() == "build-info" {
            continue;
        }

        // Check if it's a directory
        if path.is_dir() {
            // Look for .json files inside the directory
            for file in fs::read_dir(&path)? {
                let file = file?;
                let file_path = file.path();

                // Check if it's a JSON file
                if file_path.extension().and_then(|s| s.to_str()) == Some("json") {
                    // Read and parse the JSON file
                    let content = fs::read_to_string(&file_path)?;
                    match serde_json::from_str::<ConfigurableContractArtifact>(&content) {
                        Ok(artifact) => {
                            artifacts.push((file_path, artifact));
                        }
                        Err(e) => {
                            println!("Failed to parse artifact at {:?}: {}", file_path, e);
                            continue;
                        }
                    }
                }
            }
        }
    }

    Ok(artifacts)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
    pub end_line: Option<usize>,
    pub end_column: Option<usize>,
    pub start_offset: usize,
    pub length: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct BytecodeMap {
    pub source_map: SourceMap,
    pub _pc_ic_map: IcPcMap,
    pub ic_pc_map: PcIcMap,
}

impl BytecodeMap {
    pub fn new(bytecode: &CompactBytecode) -> Self {
        let source_map = parse(&bytecode.clone().source_map.unwrap()).unwrap();
        let pc_ic_map = IcPcMap::new(bytecode.bytes().unwrap());
        let ic_pc_map = PcIcMap::new(bytecode.bytes().unwrap());

        Self {
            source_map,
            _pc_ic_map: pc_ic_map,
            ic_pc_map,
        }
    }
}

struct StatementVisitor {
    pub source: String,
    pub debug_unit: DebugUnit,

    // in_constructor signals if we are in the constructor of the contract
    pub in_constructor: bool,

    // reference to the contract node that we are visiting
    pub contract_node: Option<Node>,
}

#[derive(Debug, Clone, PartialEq)]
enum StatementVisitorError {
    #[allow(dead_code)]
    ParseError,
    MissingAttribute(String),
    IncorrectType(NodeType, NodeType),
}

impl Display for StatementVisitorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatementVisitorError::ParseError => write!(f, "ParseError"),
            StatementVisitorError::MissingAttribute(attribute_name) => {
                write!(f, "MissingAttribute: {}", attribute_name)
            }
            StatementVisitorError::IncorrectType(expected, actual) => write!(
                f,
                "IncorrectType: expected {:?}, got {:?}",
                expected, actual
            ),
        }
    }
}

impl std::error::Error for StatementVisitorError {}

trait OptionExt<T> {
    fn ok_or_missing_attribute(self, attribute_name: &str) -> StatementVisitorResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn ok_or_missing_attribute(self, attribute_name: &str) -> StatementVisitorResult<T> {
        self.ok_or(StatementVisitorError::MissingAttribute(
            attribute_name.to_string(),
        ))
    }
}

type StatementVisitorResult<T> = Result<T, StatementVisitorError>;

impl StatementVisitor {
    pub fn new(
        deployed_bytecode: CompactBytecode,
        bytecode: CompactBytecode,
        path: String,
        source: String,
    ) -> Self {
        Self {
            source: source.clone(),
            in_constructor: false,
            contract_node: None,
            debug_unit: DebugUnit {
                source_id: 0,
                name: String::new(),
                path: path.clone(),
                functions: HashMap::new(),
                variables: Vec::new(),
                bytecode: BytecodeMap::new(&bytecode),
                deployed_bytecode: BytecodeMap::new(&deployed_bytecode),
            },
        }
    }

    pub fn visit_contract(&mut self, node: &Node) -> StatementVisitorResult<()> {
        self.contract_node = Some(node.clone());

        for node in &node.nodes {
            match node.node_type {
                NodeType::FunctionDefinition => {
                    let kind = node
                        .attribute::<String>("kind")
                        .ok_or_missing_attribute("kind")?;

                    let function_kind = if kind == "constructor" {
                        FunctionKind::Constructor
                    } else {
                        FunctionKind::Function
                    };

                    let function = self.build_debug_function(node, function_kind)?;
                    self.debug_unit
                        .functions
                        .insert(function.name.clone(), function);
                }
                NodeType::VariableDeclaration => {
                    let state_variable = node
                        .attribute::<bool>("stateVariable")
                        .ok_or_missing_attribute("stateVariable")?;

                    if state_variable {
                        let var = self.build_debug_variable(node)?;
                        self.debug_unit.variables.push(var.unwrap());
                    }
                }
                NodeType::StructDefinition => {}
                NodeType::EventDefinition => {}
                NodeType::ModifierDefinition => {
                    let root_block = if let Some(body) = &node.body {
                        self.build_debug_block(body)?
                    } else {
                        Block::default()
                    };

                    let func = Function {
                        name: node.attribute("name").ok_or_missing_attribute("name")?,
                        kind: FunctionKind::Modifier,
                        root_block,
                        parameters: Vec::new(),
                        loc: SourceLocationHelper {
                            start: node.src.start,
                            length: node.src.length.unwrap(),
                        },
                    };

                    self.debug_unit.functions.insert(func.name.clone(), func);
                }
                NodeType::EnumDefinition => {
                    // TODO
                }
                _ => {
                    panic!("Not handled {:?}", node.node_type);
                }
            }
        }
        Ok(())
    }

    fn source_location_for(&self, loc: &ast::LowFidelitySourceLocation) -> SourceLocation {
        // Get the substring up to the start position to count lines
        let source_until_start = &self.source[..loc.start];
        let lines_until_start: Vec<&str> = source_until_start.lines().collect();
        let start_line = lines_until_start.len();

        // Calculate start column by finding the last newline before start position
        let start_column = if let Some(last_newline_pos) = source_until_start.rfind('\n') {
            loc.start - last_newline_pos - 1
        } else {
            loc.start // If no newline found, column is same as position
        };

        // If we have a length, calculate end position
        if let Some(length) = loc.length {
            let end_pos = loc.start + length;
            let source_until_end = &self.source[..end_pos];
            let lines_until_end: Vec<&str> = source_until_end.lines().collect();
            let end_line = lines_until_end.len();

            // Calculate end column similarly to start column
            let end_column = if let Some(last_newline_pos) = source_until_end.rfind('\n') {
                end_pos - last_newline_pos - 1
            } else {
                end_pos // If no newline found, column is same as position
            };

            SourceLocation {
                line: start_line,
                column: start_column,
                end_line: Some(end_line),
                end_column: Some(end_column),
                start_offset: loc.start,
                length: Some(length),
            }
        } else {
            // If no length is provided, don't include end positions
            SourceLocation {
                line: start_line,
                column: start_column,
                end_line: None,
                end_column: None,
                start_offset: loc.start,
                length: None,
            }
        }
    }

    fn build_debug_function(
        &mut self,
        node: &Node,
        kind: FunctionKind,
    ) -> StatementVisitorResult<Function> {
        let is_constructor = matches!(kind, FunctionKind::Constructor);
        self.in_constructor = is_constructor;

        let name = match kind {
            FunctionKind::Constructor => "constructor".to_string(),
            FunctionKind::Function => node.attribute("name").ok_or_missing_attribute("name")?,
            _ => {
                unreachable!("unhandled function kind {:?}", kind);
            }
        };

        let parameters = self.build_debug_parameters(node)?;

        let root_block = if let Some(body) = &node.body {
            self.build_debug_block(body)?
        } else {
            Block::default()
        };

        let function = Function {
            name,
            root_block,
            kind,
            parameters,
            loc: SourceLocationHelper {
                start: node.src.start,
                length: node.src.length.unwrap(),
            },
        };

        self.in_constructor = false;
        Ok(function)
    }

    fn build_debug_block(&mut self, node: &Node) -> StatementVisitorResult<Block> {
        let mut block = Block {
            variables: Vec::new(),
            condition: None,
            instructions: Vec::new(),
            scopes: Vec::new(),
            location: self.source_location_for(&node.src),
        };

        let statements: Vec<Node> = node.attribute("statements").unwrap_or_default();
        for statement in &statements {
            match statement.node_type {
                NodeType::ExpressionStatement => {
                    let block_location = self.source_location_for(&statement.src);

                    // Process for function calls within the expression
                    // It is important this one takes precedence over the regular statemtent
                    // If the statement si only the function call, they are going to share the same srcmap
                    // (maybe we could check that directly too). If they statement is put before the function call
                    // the match selector during tracing will pick the statement instead of the function call always
                    // and we will not be able to detect the function call at all.
                    if let Some(expr) = statement.attribute::<Node>("expression") {
                        self.process_expression_for_function_calls(
                            &expr,
                            &mut block,
                            &block_location,
                        )?;
                    }

                    block.instructions.push(Instruction {
                        location: block_location.clone(),
                        kind: InstructionKind::Statement,
                        loc: SourceLocationHelper {
                            start: statement.src.start,
                            length: statement.src.length.unwrap(),
                        },
                    });
                }
                NodeType::IfStatement => {
                    // Create single block for if statement body
                    if let Some(true_body) = statement.attribute("trueBody") {
                        let mut if_block = self.build_debug_block(&true_body)?;
                        // Add the condition to the block
                        if let Some(condition) = statement.attribute::<Node>("condition") {
                            if_block.condition = Some(Instruction {
                                location: self.source_location_for(&condition.src),
                                kind: InstructionKind::Statement,
                                loc: SourceLocationHelper {
                                    start: condition.src.start,
                                    length: condition.src.length.unwrap(),
                                },
                            });
                        }
                        block.scopes.push(if_block);
                    }
                }
                NodeType::ForStatement => {
                    // Create single block for for loop body
                    if let Some(body) = &statement.body {
                        let mut for_block = self.build_debug_block(body)?;
                        // Add the condition to the block
                        if let Some(condition) = statement.attribute::<Node>("condition") {
                            for_block.condition = Some(Instruction {
                                location: self.source_location_for(&condition.src),
                                kind: InstructionKind::Statement,
                                loc: SourceLocationHelper {
                                    start: condition.src.start,
                                    length: condition.src.length.unwrap(),
                                },
                            });
                        }
                        block.scopes.push(for_block);
                    }
                }
                NodeType::VariableDeclarationStatement => {
                    let var = self.build_debug_variable(statement)?.expect("variable");
                    self.debug_unit.variables.push(var.clone());
                    block.variables.push(var.id as usize);

                    let block_location = self.source_location_for(&statement.src);

                    block.instructions.push(Instruction {
                        location: block_location.clone(),
                        kind: InstructionKind::VariableDeclaration(var.id as usize),
                        loc: SourceLocationHelper {
                            start: statement.src.start,
                            length: statement.src.length.unwrap(),
                        },
                    });

                    // parse the initial value for other expressions because it might include a function call that we need to parse
                    if let Some(expr) = statement.attribute::<Node>("initialValue") {
                        self.process_expression_for_function_calls(
                            &expr,
                            &mut block,
                            &block_location,
                        )?;
                    }
                }
                _ => {
                    // Regular statement
                    block.instructions.push(Instruction {
                        location: self.source_location_for(&statement.src),
                        kind: InstructionKind::Statement,
                        loc: SourceLocationHelper {
                            start: statement.src.start,
                            length: statement.src.length.unwrap(),
                        },
                    });
                }
            }
        }

        Ok(block)
    }

    fn process_expression_for_function_calls(
        &self,
        node: &Node,
        block: &mut Block,
        block_location: &SourceLocation,
    ) -> StatementVisitorResult<()> {
        match node.node_type {
            NodeType::FunctionCall => {
                // Skip internal functions (those with negative referencedDeclaration)
                if let Some(expr) = node.attribute::<Node>("expression") {
                    if let Some(ref_decl) = expr.attribute::<i32>("referencedDeclaration") {
                        if ref_decl < 0 {
                            // Skip this function call as it's an internal function
                            return Ok(());
                        }
                    }
                }

                // Add function call instruction using block's location
                block.instructions.push(Instruction {
                    location: block_location.clone(),
                    kind: InstructionKind::FunctionCall,
                    loc: SourceLocationHelper {
                        start: node.src.start,
                        length: node.src.length.unwrap(),
                    },
                });

                // Still process arguments for nested function calls
                if let Some(args) = node.attribute::<Vec<Node>>("arguments") {
                    for arg in args {
                        self.process_expression_for_function_calls(&arg, block, block_location)?;
                    }
                }
            }
            NodeType::BinaryOperation => {
                if let Some(left) = node.attribute::<Node>("leftExpression") {
                    self.process_expression_for_function_calls(&left, block, block_location)?;
                }
                if let Some(right) = node.attribute::<Node>("rightExpression") {
                    self.process_expression_for_function_calls(&right, block, block_location)?;
                }
            }
            NodeType::Assignment => {
                if let Some(right) = node.attribute::<Node>("rightHandSide") {
                    self.process_expression_for_function_calls(&right, block, block_location)?;
                }
            }
            NodeType::UnaryOperation => {
                if let Some(sub) = node.attribute::<Node>("subExpression") {
                    self.process_expression_for_function_calls(&sub, block, block_location)?;
                }
            }
            NodeType::MemberAccess | NodeType::IndexAccess => {
                if let Some(expr) = node.attribute::<Node>("expression") {
                    self.process_expression_for_function_calls(&expr, block, block_location)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn build_debug_variable(&self, node: &Node) -> StatementVisitorResult<Option<Variable>> {
        if let Some(name) = node.attribute("name") {
            // this is most likely a state variable
            if let Some(id) = node.id {
                return Ok(Some(Variable {
                    name,
                    id: id as u64,
                    location: self.source_location_for(&node.src),
                    state_variable: true,
                }));
            }
        } else {
            // check now for a normal varaible decalration
            let declarations = node
                .attribute::<Vec<Node>>("declarations")
                .ok_or_missing_attribute("declarations")?;

            let declaration = declarations.first().unwrap();

            let name = declaration
                .attribute::<String>("name")
                .ok_or_missing_attribute("name")?;

            return Ok(Some(Variable {
                name,
                id: node.id.unwrap() as u64,
                location: self.source_location_for(&node.src),
                state_variable: false,
            }));
        }
        Ok(None)
    }

    fn build_debug_parameters(&mut self, node: &Node) -> StatementVisitorResult<Vec<Variable>> {
        let mut parameters = Vec::new();

        let parameters_list = node
            .attribute::<Node>("parameters")
            .ok_or_missing_attribute("parameters")?;

        if parameters_list.node_type != NodeType::ParameterList {
            return Err(StatementVisitorError::IncorrectType(
                NodeType::ParameterList,
                parameters_list.node_type,
            ));
        }

        let params = parameters_list
            .attribute::<Vec<Node>>("parameters")
            .ok_or_missing_attribute("parameters")?;

        for param in params {
            if param.node_type != NodeType::VariableDeclaration {
                return Err(StatementVisitorError::IncorrectType(
                    NodeType::VariableDeclaration,
                    param.node_type,
                ));
            }

            if let Some(var) = self.build_debug_variable(&param)? {
                let mut var = var.clone();
                var.state_variable = false;

                self.debug_unit.variables.push(var.clone());
                parameters.push(var);
            }
        }

        Ok(parameters)
    }
}

#[derive(Debug, Clone)]
pub struct PcIcMap {
    pub inner: HashMap<usize, usize>,
}
impl PcIcMap {
    /// Creates a new `IcPcMap` for the given code.
    pub fn new(code: &[u8]) -> Self {
        Self {
            inner: make_map::<true>(code),
        }
    }

    /// Returns the instruction counter for the given program counter.
    pub fn get(&self, pc: usize) -> Option<usize> {
        self.inner.get(&pc).copied()
    }
}

/// Maps from program counter to instruction counter.
#[derive(Debug, Clone)]
pub struct IcPcMap {
    pub inner: HashMap<usize, usize>,
}

impl IcPcMap {
    /// Creates a new `IcPcMap` for the given code.
    pub fn new(code: &[u8]) -> Self {
        Self {
            inner: make_map::<false>(code),
        }
    }

    /// Returns the instruction counter for the given program counter.
    pub fn get(&self, pc: usize) -> Option<usize> {
        self.inner.get(&pc).copied()
    }
}

fn make_map<const PC_FIRST: bool>(code: &[u8]) -> HashMap<usize, usize> {
    let mut map = HashMap::default();

    let mut pc = 0;
    let mut cumulative_push_size = 0;
    while pc < code.len() {
        let ic = pc - cumulative_push_size;
        if PC_FIRST {
            map.insert(pc, ic);
        } else {
            map.insert(ic, pc);
        }

        // Check if current byte is a PUSH operation (0x60-0x7f)
        let current_byte = code[pc];
        if (0x60..=0x7f).contains(&current_byte) {
            // Calculate push size: for PUSH1 (0x60) it's 1, for PUSH32 (0x7f) it's 32
            let push_size = (current_byte - 0x60 + 1) as usize;
            pc += push_size;
            cumulative_push_size += push_size;
        }

        pc += 1;
    }
    map
}

#[derive(Debug, Clone)]
pub enum MatchResult {
    Function(Function),
    FunctionWithOut(Function),
    Instruction(Instruction),
    ConstructorOut,
}

impl MatchResult {
    fn kind(&self) -> &str {
        match self {
            MatchResult::Function(_) => "function",
            MatchResult::FunctionWithOut(_) => "function_out",
            MatchResult::Instruction(_) => "instruction",
            MatchResult::ConstructorOut => "constructor_out",
        }
    }

    fn get_source_element(&self) -> &SourceLocationHelper {
        match self {
            MatchResult::Function(func) => &func.loc,
            MatchResult::FunctionWithOut(func) => &func.loc,
            MatchResult::Instruction(inst) => &inst.loc,
            MatchResult::ConstructorOut => panic!("constructor out has no source element b"),
        }
    }
}

// Conversion from your existing structures
impl DebugUnit {
    pub fn match_location(&self, loc: &SourceElement) -> (Option<MatchResult>, Vec<usize>) {
        fn search_block(
            block: &Block,
            loc: &SourceElement,
            parent_vars: Vec<usize>,
        ) -> (Option<MatchResult>, Vec<usize>) {
            let mut vars_in_scope = parent_vars.clone();

            for inst in &block.instructions {
                if let InstructionKind::VariableDeclaration(id) = inst.kind {
                    vars_in_scope.push(id);
                };

                if inst.loc.matches(loc) {
                    //if loc.offset() == 844 || loc.offset() == 881 {
                    //    if !matches!(inst.kind, InstructionKind::FunctionCall) {
                    //        continue;
                    //    }
                    //}

                    return (Some(MatchResult::Instruction(inst.clone())), vars_in_scope);
                }
            }

            for scope in &block.scopes {
                if let (Some(result), vars) = search_block(scope, loc, vars_in_scope.clone()) {
                    return (Some(result), vars);
                }
            }

            // cond not done yet?
            if let Some(cond) = &block.condition {
                if cond.loc.matches(loc) {
                    return (Some(MatchResult::Instruction(cond.clone())), vars_in_scope);
                }
            }

            (None, vec![])
        }

        // try to find exact match by brute force for now
        for func in self.functions.values() {
            // Initialize vars_in_scope with state variable IDs
            let mut vars_in_scope: Vec<usize> = self
                .variables
                .iter()
                .filter(|v| v.state_variable)
                .map(|v| v.id as usize)
                .collect();

            // add variables from the function parameters
            for func in func.parameters.iter() {
                vars_in_scope.push(func.id as usize);
            }

            if func.loc.matches(loc) {
                return (Some(MatchResult::Function(func.clone())), vars_in_scope);
            }

            if let (Some(result), vars) = search_block(&func.root_block, loc, vars_in_scope) {
                return (Some(result), vars);
            }
        }

        (None, vec![])
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub enum StepKind {
    FunctionDefinition(String),
    FunctionCall,
    ConstructorOut,

    #[default]
    Statement,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DebugTrace {
    pub steps: Vec<DebugStep>,
    pub variables: HashMap<u64, Variable>,
}

#[derive(Debug, Clone)]
pub struct StackFrame {
    pub path: String,
    pub location: SourceLocation,
}

#[derive(Debug, Clone)]
pub struct DebugTraceStep {
    pub stack_frames: Vec<StackFrame>,
}

impl DebugTrace {
    pub fn trace(&self, indx: usize) -> DebugTraceStep {
        let mut call_trace = Vec::new();
        let step = self.steps.get(indx).unwrap();

        // retrieve the call trace for this step
        for step_trace in step.call_trace.iter() {
            let parent_step = self.steps.get(*step_trace).unwrap();
            call_trace.push(StackFrame::from(parent_step));
        }

        // now add the current step to the call trace
        call_trace.push(StackFrame::from(step));

        DebugTraceStep {
            stack_frames: call_trace,
        }
    }

    pub fn scope(&self, indx: usize) -> Vec<Variable> {
        // so the frame id is the id in the call trace
        // for now lets keep it simple and assume it is the current step
        let step = self.steps.get(indx).unwrap();
        step.variables_in_scope
            .iter()
            .map(|id| self.variables.get(&(*id as u64)).unwrap().clone())
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DebugStep {
    pub location: SourceLocation,
    pub variables_in_scope: Vec<usize>,
    pub path: String,
    pub call_trace: Vec<usize>,
    pub kind: StepKind,
    pub memory: Bytes,
    pub stack: Vec<Bytes>,
    pub storage: HashMap<Bytes, Bytes>,
}

impl From<&DebugStep> for StackFrame {
    fn from(step: &DebugStep) -> Self {
        StackFrame {
            location: step.location.clone(),
            path: step.path.clone(),
        }
    }
}

#[derive(Error, Debug)]
pub enum TraceError {
    #[error("Failed to read file: {0} {1}")]
    FailedToReadFile(String, std::io::Error),

    #[error("Failed to parse debug dump JSON: {0} {1}")]
    FailedToParseDebugDump(String, serde_json::Error),

    #[error("Found function entry without call")]
    FoundFunctionEntryWithoutCall,

    #[error("Found function exit without call")]
    FoundFunctionExitWithoutCall,

    #[error("Found instruction without function entry")]
    FoundInstructionWithoutFunctionEntry,

    #[error("Last step should have call trace equal to 0")]
    LastStepShouldHaveCallTraceEqualZero,

    #[error("Function with incorrect exit pc: {0} {1}")]
    FunctionWithIncorrectExitPc(String, String),
}

fn source_element_matches(a: &SourceElement, b: &SourceElement) -> bool {
    a.offset() == b.offset() && a.length() == b.length() && a.index_i32() == b.index_i32()
}

#[derive(Debug, Clone)]
struct OtherMatchLocation {
    match_result: MatchResult,
    source_location: SourceElement,
    path: String,
    vars_in_scope: Vec<usize>,
}

pub fn generate_trace(workspace_path: &str, trace_path: &str) -> Result<DebugTrace, TraceError> {
    let content = fs::read_to_string(trace_path)
        .map_err(|e| TraceError::FailedToReadFile(trace_path.to_string(), e))?;

    let context: DebuggerContext = serde_json::from_str(&content)
        .map_err(|e| TraceError::FailedToParseDebugDump(trace_path.to_string(), e))?;

    let contracts_involved: HashSet<String> = context
        .contracts
        .identified_contracts
        .values()
        .cloned()
        .collect();

    println!("contracts involved {:?}", contracts_involved);

    let root_path = Path::new(workspace_path);
    let debug_units = generate_debug_units(root_path, None).unwrap();

    // map each contract to its current storage
    let mut contracts_storage = HashMap::new();

    // get all the debug unit and sort them out by source id
    let mut debug_units_by_source_id = HashMap::new();
    for (_, dd) in debug_units.iter() {
        debug_units_by_source_id
            .entry(dd.source_id)
            .or_insert_with(Vec::new)
            .push(dd.clone());
    }

    let mut matched_locations = Vec::new();

    for node in context.debug_arena.iter() {
        // name of the contract in this step
        let contract = context
            .contracts
            .identified_contracts
            .get(&node.address)
            .unwrap();

        let debug_unit = debug_units.get(contract).unwrap();

        for step in node.steps.iter() {
            let memory = Bytes::from(step.memory.clone().unwrap().as_bytes().to_vec());
            let stack: Vec<Bytes> = step
                .stack
                .clone()
                .unwrap()
                .iter()
                .map(|b| Bytes::from(b.as_le_bytes().to_vec()))
                .collect();

            // the storage for the current state is before any key has been inserted
            let storage = contracts_storage
                .get(&step.contract)
                .unwrap_or(&HashMap::new())
                .clone();

            // storage always have to be processed
            if let Some(storage_change) = step.storage_change {
                contracts_storage
                    .entry(step.contract)
                    .or_insert(HashMap::new())
                    .insert(
                        Bytes::from(storage_change.key.as_le_bytes().to_vec()),
                        Bytes::from(storage_change.value.as_le_bytes().to_vec()),
                    );
            }

            let pc = step.pc;

            let bytecode = if node.kind.is_create() {
                &debug_unit.bytecode
            } else {
                &debug_unit.deployed_bytecode
            };

            let ic_index = bytecode.ic_pc_map.get(pc).unwrap();
            let source_location = bytecode.source_map.get(ic_index).unwrap();
            let source_id = source_location.index_i32() as u32;

            println!(
                "pc {:?} map {:?} srcmap {:?} jump {:?}",
                pc,
                ic_index,
                source_location,
                source_location.jump(),
            );

            let debug_units_to_test =
                if let Some(debug_unit) = debug_units_by_source_id.get(&source_id) {
                    debug_unit
                } else {
                    continue;
                };

            for debug_unit_to_test in debug_units_to_test.iter() {
                if let (Some(loc), vars) = debug_unit_to_test.match_location(source_location) {
                    println!("location {:?}", loc.kind());
                    println!("vars {:?}", vars);

                    matched_locations.push(OtherMatchLocation {
                        match_result: loc,
                        source_location: source_location.clone(),
                        path: debug_unit_to_test.path.clone(),
                        vars_in_scope: vars,
                    });
                } else {
                    println!("cannot find location");
                }
            }

            // find the mapping source location for this pc

            /*
            // for all the functions, find the entry pc
            for func in debug_unit.functions.values() {
                if func.entry_pc == step.pc
                    && (func.kind == FunctionKind::Function
                        // Constructor is only valid if the trace is for a create operation
                        || (func.kind == FunctionKind::Constructor && node.kind.is_create()))
                {
                    if !expecting_function {
                        return Err(TraceError::FoundFunctionEntryWithoutCall);
                    }
                    expecting_function = false;

                    let is_first_step = steps.is_empty();

                    steps.push(DebugStep {
                        location: func.root_block.location.clone(),
                        path: debug_unit.path.clone(),
                        memory: memory.clone(),
                        stack: stack.clone(),
                        storage: storage.clone(),
                        variables_in_scope: vec![],
                        call_trace: call_trace.iter().map(|(_, pos)| *pos).collect(),
                        kind: StepKind::FunctionDefinition(func.name.clone()),
                    });

                    // add the entry to the call trace alongside its position in the steps vector
                    if !is_first_step {
                        // we do not add any for the root function call
                        call_trace.push((func, steps.len() - 1));
                    }
                }
            }
            // same with exit pc
            for func in debug_unit.functions.values() {
                if func.exit_pc == step.pc
                    && (func.kind == FunctionKind::Function
                        || (func.kind == FunctionKind::Constructor
                            && node.kind.is_create()))
                {
                    if expecting_function {
                        return Err(TraceError::FoundFunctionExitWithoutCall);
                    }

                    // pop the last call trace and make sure the function is the same
                    let last_call = call_trace.pop();
                    if let Some(last_call) = last_call {
                        if last_call.0.name.clone() != func.name {
                            return Err(TraceError::FunctionWithIncorrectExitPc(
                                func.name.clone(),
                                func.exit_pc,
                                last_call.0.name.clone(),
                            ));
                        }
                    }
                }
            }

            if let (Some(instruction), variables_in_scope) =
                debug_unit.get_location_at_pc(step.pc)
            {
                // get a list of all the calltrace ids
                if expecting_function {
                    return Err(TraceError::FoundInstructionWithoutFunctionEntry);
                }

                println!("instruction {:?}", instruction);

                if matches!(instruction.kind, InstructionKind::FunctionCall) {
                    expecting_function = true;

                    steps.push(DebugStep {
                        location: instruction.location.clone(),
                        path: debug_unit.path.clone(),
                        variables_in_scope,
                        memory: memory.clone(),
                        stack,
                        storage,
                        call_trace: call_trace.iter().map(|(_, pos)| *pos).collect(),
                        kind: StepKind::FunctionCall,
                    });
                } else {
                    steps.push(DebugStep {
                        location: instruction.location.clone(),
                        path: debug_unit.path.clone(),
                        variables_in_scope,
                        memory: memory.clone(),
                        stack,
                        storage,
                        call_trace: call_trace.iter().map(|(_, pos)| *pos).collect(),
                        kind: StepKind::Statement,
                    });
                }
            }
            */
            //}
            // }
        }

        if node.kind.is_create() {
            // we have to put a new "fake" instruction to signal that we got out of the constructor because
            // the way the constructor is laid out in the srcmap/pc is srcmap of the contract call
            // and then srcmap of internal elements but not a srcmap for the out call for this contract
            // so we have to add a new instruction to signal that we got out of the constructor
            matched_locations.push(OtherMatchLocation {
                match_result: MatchResult::ConstructorOut,
                source_location: Default::default(),
                path: "".to_string(),
                vars_in_scope: vec![],
            });
        }
    }

    println!("UNCLEAN VERSION");
    for i in matched_locations.iter() {
        println!(
            "matched location {:?} {:?}",
            i.source_location,
            i.match_result.kind()
        );
    }

    let chunks = matched_locations
        .linear_group_by(|a, b| source_element_matches(&a.source_location, &b.source_location));
    let mut final_matched_locations = Vec::new();

    for chunk in chunks {
        // figure out the first entry to know how we are going to match and process this chunk
        let first_entry = chunk.first().unwrap();

        match &first_entry.match_result {
            MatchResult::Function(_) => {
                // find the element with the Out if there is any, that signals the function exit
                let func_with_out = chunk.iter().find(|i| i.source_location.jump() == Jump::Out);

                if let Some(func_with_out) = func_with_out {
                    let func_core = match &func_with_out.match_result {
                        MatchResult::Function(func) => func,
                        _ => panic!("this is not expected to happen"),
                    };
                    final_matched_locations.push(OtherMatchLocation {
                        match_result: MatchResult::FunctionWithOut(func_core.clone()),
                        source_location: func_with_out.source_location.clone(), // we care less about this ones
                        path: func_with_out.path.clone(),
                        vars_in_scope: vec![],
                    });
                } else {
                    // otherwise, pick the last element that matches
                    let last_element = chunk.last().unwrap();
                    final_matched_locations.push(last_element.clone());
                }
            }
            MatchResult::Instruction(inst) => {
                if matches!(inst.kind, InstructionKind::FunctionCall) {
                    // for function calls we must have a function with in, because whena  function call happens
                    // you have some sourcemap pc pointer when it returns that we do not need, so in order to filter that, we keep the chunk
                    // with the in
                    let does_have_in = chunk.iter().find(|i| i.source_location.jump() == Jump::In);
                    if does_have_in.is_none() {
                        continue;
                    }
                }

                // just get the first instruction
                final_matched_locations.push(first_entry.clone());
            }
            MatchResult::ConstructorOut => {
                // push it as it is, there should only be one of this
                final_matched_locations.push(first_entry.clone());
            }
            MatchResult::FunctionWithOut(_) => {
                panic!("this is not expected to happen")
            }
        }
    }

    println!("show final matched locations");
    for i in final_matched_locations.iter() {
        println!("=> {:?}", i.match_result.kind());
    }

    let mut new_debug_steps = Vec::new();
    let mut call_trace = Vec::new();
    let mut expecting_function = true;

    for i in final_matched_locations.iter() {
        let local_call_trace = call_trace.iter().map(|(_, pos)| *pos).collect();
        let path = i.path.clone();

        match &i.match_result {
            MatchResult::Function(func) => {
                println!("function {:?}", func.name);
                if !expecting_function {
                    return Err(TraceError::FoundFunctionEntryWithoutCall);
                }
                expecting_function = false;

                let is_first_step = new_debug_steps.is_empty();
                if !is_first_step {
                    call_trace.push((func, new_debug_steps.len() - 1));
                }

                new_debug_steps.push(DebugStep {
                    location: func.root_block.location.clone(),
                    path,
                    variables_in_scope: vec![],
                    call_trace: local_call_trace,
                    kind: StepKind::FunctionDefinition(func.name.clone()),
                    ..Default::default()
                });
            }
            MatchResult::ConstructorOut => {
                println!("constructor out");
                call_trace.pop();
            }
            MatchResult::FunctionWithOut(func) => {
                println!("function with out {:?}", func.name);

                // pop the last call trace and make sure the function is the same
                let last_call = call_trace.pop();
                if let Some(last_call) = last_call {
                    if last_call.0.name.clone() != func.name {
                        return Err(TraceError::FunctionWithIncorrectExitPc(
                            func.name.clone(),
                            last_call.0.name.clone(),
                        ));
                    }
                }
            }
            MatchResult::Instruction(inst) => {
                let stmt_kind = match inst.kind {
                    InstructionKind::FunctionCall => StepKind::FunctionCall,
                    _ => StepKind::Statement,
                };

                new_debug_steps.push(DebugStep {
                    location: inst.location.clone(),
                    path,
                    variables_in_scope: i.vars_in_scope.clone(),
                    call_trace: local_call_trace,
                    kind: stmt_kind,
                    ..Default::default()
                });

                if matches!(inst.kind, InstructionKind::FunctionCall) {
                    expecting_function = true;
                }
            }
        }
        // println!("i => {:?}", i);
    }

    // the last step should have call trace equal to 0
    if !new_debug_steps.last().unwrap().call_trace.is_empty() {
        tracing::warn!("last step has call trace");
        //     return Err(TraceError::LastStepShouldHaveCallTraceEqualZero);
    }

    // loop over all the debug units and get the variable definitions
    let mut variable_definitions = HashMap::new();
    for debug_unit in debug_units.values() {
        for variable in debug_unit.variables.iter() {
            println!("inserting variable {:?}", variable.id);
            variable_definitions.insert(variable.id, variable.clone());
        }
    }

    let final_trace = DebugTrace {
        steps: new_debug_steps,
        variables: variable_definitions,
    };

    return Ok(final_trace);

    println!("final_trace => {:?}", final_trace);

    // loop over all the debug units and get the variable definitions
    let mut variable_definitions = HashMap::new();
    for debug_unit in debug_units.values() {
        for variable in debug_unit.variables.iter() {
            variable_definitions.insert(variable.id, variable.clone());
        }
    }

    Ok(DebugTrace {
        steps,
        variables: variable_definitions,
    })
}

#[derive(Debug, Clone)]
pub struct DebugUnit {
    pub name: String,
    pub path: String,
    pub functions: HashMap<String, Function>,
    pub variables: Vec<Variable>,
    pub source_id: u32,
    pub bytecode: BytecodeMap,
    pub deployed_bytecode: BytecodeMap,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FunctionKind {
    Constructor,
    Function,
    Modifier,
}

#[derive(Debug, Clone, PartialEq)]
struct SourceLocationHelper {
    pub start: usize,
    pub length: usize,
}

impl SourceLocationHelper {
    pub fn matches(&self, loc: &SourceElement) -> bool {
        self.start == loc.offset() as usize && self.length == loc.length() as usize
    }
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub kind: FunctionKind,
    pub root_block: Block,
    pub parameters: Vec<Variable>,
    pub loc: SourceLocationHelper,
}

#[derive(Debug, Clone, Default)]
pub struct Block {
    pub variables: Vec<usize>,
    // For if conditions and loop conditions
    pub condition: Option<Instruction>,
    // The actual statements in this block
    pub instructions: Vec<Instruction>,
    // Nested scopes (if/else bodies, loop bodies)
    pub scopes: Vec<Block>,
    pub location: SourceLocation,
}

#[derive(Debug, Clone, Serialize)]
pub struct Variable {
    pub name: String,
    pub id: u64,
    pub location: SourceLocation,
    pub state_variable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstructionKind {
    VariableDeclaration(usize),
    Statement,
    FunctionCall,
}

#[derive(Debug, Clone)]
pub struct Instruction {
    pub location: SourceLocation,
    pub kind: InstructionKind,
    pub loc: SourceLocationHelper,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct VariableId(pub u64);

#[derive(Debug, Clone)]
pub struct CommandArgs {
    args: Vec<String>,
}

impl CommandArgs {
    fn new() -> Self {
        Self { args: Vec::new() }
    }

    fn arg(&mut self, arg: &str) -> &mut Self {
        self.args.push(arg.to_string());
        self
    }
}

impl IntoIterator for CommandArgs {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;

    fn into_iter(self) -> Self::IntoIter {
        self.args.into_iter()
    }
}

impl Display for CommandArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.args.join(" "))
    }
}

pub fn execute_command(
    workspace_path: &str,
    args: CommandArgs,
) -> std::io::Result<std::process::Output> {
    let output = Command::new("forge")
        .current_dir(workspace_path)
        .args(args.clone())
        .env("RUST_LOG", "info")
        .output()?;

    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        Err(std::io::Error::other(format!(
            "Forge command failed: {} {} {}",
            workspace_path, stdout, stderr
        )))
    }
}

pub struct Forge {}

impl Forge {
    pub fn test(function_name: &str, test_path: &str) -> CommandArgs {
        let mut cmd = CommandArgs::new();
        cmd.arg("test")
            .arg("--match-test")
            .arg(function_name)
            .arg("--match-path")
            .arg(test_path);

        cmd.clone()
    }

    pub fn debug(function_name: &str, test_path: &str, output_path: &str) -> CommandArgs {
        let mut cmd = CommandArgs::new();
        cmd.arg("test")
            .arg("--debug")
            .arg("--match-test")
            .arg(function_name)
            .arg("--match-path")
            .arg(test_path)
            .arg("--dump")
            .arg(output_path)
            .arg("--ast")
            .arg("--optimizer-runs")
            .arg("0")
            .arg("--optimize")
            .arg("false")
            .arg("-vvvvv"); // we need to run with this flag to export the storage changes

        cmd.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::compile_contract;
    use std::fmt::{Display, Write};

    impl Display for DebugUnit {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "(contract\n")?;

            // Write state variables
            write!(f, "  (state-vars\n")?;
            for var in &self.variables {
                if var.state_variable {
                    write!(f, "    {}\n", var.name)?;
                }
            }
            write!(f, "  )\n")?;

            // Sort functions by name for consistent output
            let mut sorted_functions: Vec<_> = self.functions.values().collect();
            sorted_functions.sort_by(|a, b| a.name.cmp(&b.name));

            // Write functions
            for function in sorted_functions {
                write!(f, "  (function {}\n", function.name)?;
                let param_names = function
                    .parameters
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                write!(f, "    (params {})\n", param_names)?;
                self.fmt_block(f, &function.root_block, 2)?;
                write!(f, "  )\n")?;
            }

            write!(f, ")")
        }
    }

    impl DebugUnit {
        fn fmt_block(
            &self,
            f: &mut std::fmt::Formatter<'_>,
            block: &Block,
            indent: usize,
        ) -> std::fmt::Result {
            write!(f, "{}(block\n", "  ".repeat(indent))?;

            // Write statements
            for inst in &block.instructions {
                write!(
                    f,
                    "{}(stmt {}:{})\n",
                    "  ".repeat(indent + 1),
                    inst.location.start_offset,
                    inst.location.length.unwrap_or(0)
                )?;
            }

            // Write nested scopes
            for scope in &block.scopes {
                self.fmt_block(f, scope, indent + 1)?;
            }

            write!(f, "{})\n", "  ".repeat(indent))
        }
    }

    impl DebugTrace {
        pub fn to_debug_format(&self, workspace_path: &str) -> String {
            let mut output = String::new();

            for (_i, step) in self.steps.iter().enumerate() {
                // Calculate depth based on call_trace length
                let depth = step.call_trace.len();
                let indent = "  ".repeat(depth);

                // Strip the workspace prefix from the path
                let relative_path = step
                    .path
                    .strip_prefix(workspace_path)
                    .unwrap_or(&step.path)
                    .to_string()
                    .trim_start_matches('/')
                    .to_string();

                match &step.kind {
                    StepKind::FunctionDefinition(name) => {
                        writeln!(
                            output,
                            "{}[FUNC] {} ({}:{})",
                            indent, name, relative_path, step.location.line
                        )
                        .unwrap();
                    }
                    StepKind::ConstructorOut => {
                        panic!("it is only a placeholder for the constructor out");
                    }
                    StepKind::FunctionCall => {
                        writeln!(
                            output,
                            "{}[CALL] {}:{}",
                            indent, step.location.line, step.location.column
                        )
                        .unwrap();
                    }
                    StepKind::Statement => {
                        // println!("print statement");
                        // println!("variables in scope {:?}", step.variables_in_scope);
                        // println!("variables {:?}", self.variables);

                        // Get names of variables in scope
                        let vars_in_scope: Vec<String> = step
                            .variables_in_scope
                            .iter()
                            .map(|&id| self.variables.get(&(id as u64)).unwrap())
                            .map(|var| var.name.clone())
                            .collect();

                        writeln!(
                            output,
                            "{}[STMT] {}:{} scope=[{}]",
                            indent,
                            step.location.line,
                            step.location.column,
                            vars_in_scope.join(", ")
                        )
                        .unwrap();
                    }
                }
            }
            output
        }
    }

    #[test]
    fn test_debugger_syntax() {
        let workspace_path_string = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR not set")
            + "/src/testcases";

        let workspace_path = workspace_path_string.as_str();
        let test_dir = Path::new(workspace_path).join("syntax");

        for entry in fs::read_dir(test_dir).unwrap() {
            let path = entry.unwrap().path();

            if path.is_file() && path.extension().unwrap() == "sol" {
                let expected_path = path.with_extension("syntax");

                let contract = fs::read_to_string(&path).unwrap();
                let artifact = compile_contract(&contract).unwrap();

                let deployed_bytecode = artifact.compact_bytecode_deployed();
                let bytecode = artifact.compact_bytecode();

                let contract_ast = artifact.ast.nodes.last().unwrap();

                let mut visitor = StatementVisitor::new(
                    deployed_bytecode,
                    bytecode,
                    path.to_string_lossy().to_string(),
                    contract,
                );
                visitor.visit_contract(contract_ast).unwrap();

                let debug_unit = visitor.debug_unit;
                let expected = fs::read_to_string(expected_path).unwrap();

                println!("{}", debug_unit.to_string());
                println!("{}", expected);

                // Normalize strings by trimming whitespace and removing extra spaces
                let actual = debug_unit
                    .to_string()
                    .trim()
                    .replace("  ", " ")
                    .replace(" \n", "\n");
                let expected = expected.trim().replace("  ", " ").replace(" \n", "\n");

                assert_eq!(actual, expected);
            }
        }
    }

    struct TraceTestCase {
        pub test_path: String,
        pub expected_path: PathBuf,
        pub test_case_function: String,
    }

    fn get_trace_test_cases(workspace_path: &str) -> Vec<TraceTestCase> {
        let test_dir = Path::new(workspace_path).join("test");

        // find all the foundry test cases, files that have as extension .sol
        let test_cases = fs::read_dir(test_dir)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                if path.is_file() && path.extension().unwrap() == "sol" {
                    Some(path)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        // For each of the test cases, find the traces.
        // The trace files are in the same folder with the following format:
        // <test_case_name>.t.sol.<func_name>.trace
        // where <func_name> is the internal function in the test that they trace.
        let mut trace_test_cases = Vec::new();

        test_cases.iter().for_each(|test_path| {
            let trace_files = fs::read_dir(test_path.parent().unwrap())
                .unwrap()
                .filter_map(|entry| {
                    let path = entry.unwrap().path();
                    if path.is_file() && path.extension().unwrap() == "trace" {
                        let trace_name = path.to_string_lossy().to_string();
                        let trace_name = trace_name.split('.').nth_back(1).unwrap();

                        Some(TraceTestCase {
                            test_path: test_path.to_string_lossy().to_string(),
                            expected_path: path,
                            test_case_function: trace_name.to_string(),
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();

            trace_test_cases.push(trace_files);
        });

        let trace_test_cases: Vec<_> = trace_test_cases.into_iter().flatten().collect();
        trace_test_cases
    }

    #[test]
    fn test_debugger_traces() -> eyre::Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        let workspace_path_string = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR not set")
            + "/src/testcases";
        let workspace_path = workspace_path_string.as_str();

        let filter_trace = std::env::var("FILTER_TRACE").unwrap_or_default();

        let test_traces = get_trace_test_cases(workspace_path);
        for trace_test_case in test_traces {
            // If the FILTER_TRACE env variable is set, only run the trace for the function
            // specified there.
            if !filter_trace.is_empty() && filter_trace != trace_test_case.test_case_function {
                continue;
            }

            // write the metadata information
            let log_output_dir = format!("trace/{}", timestamp);
            std::fs::create_dir_all(&log_output_dir)?;

            let debug_trace_path = {
                let path = std::path::absolute(format!("{}/forge_trace.json", log_output_dir))
                    .expect("Failed to get absolute path");
                path.to_string_lossy().into_owned()
            };

            let forge = Forge::debug(
                &trace_test_case.test_case_function,
                &trace_test_case.test_path,
                debug_trace_path.as_str(),
            );
            let _ = execute_command(workspace_path, forge).unwrap();

            let debug_trace = generate_trace(workspace_path, debug_trace_path.as_str()).unwrap();

            // save the debug trace
            let debug_trace_json = serde_json::to_string(&debug_trace).unwrap();
            std::fs::write(
                format!("{}/debug_trace.json", log_output_dir),
                debug_trace_json,
            )?;

            let formatted = debug_trace.to_debug_format(workspace_path);
            std::fs::write(
                format!("{}/debug_trace.txt", log_output_dir),
                formatted.clone(),
            )?;

            let expected = fs::read_to_string(trace_test_case.expected_path).unwrap();

            println!("{}", formatted);
            println!("{}", expected);

            assert_eq!(formatted, expected);
        }

        Ok(())
    }
}
