use alloy_primitives::Address;
use alloy_primitives::Bytes;
use foundry_compilers::artifacts::ast::{self, Node, NodeType};
use foundry_compilers::artifacts::sourcemap::Jump;
use foundry_compilers::artifacts::sourcemap::{parse, SourceMap};
use foundry_compilers::artifacts::CompactBytecode;
use foundry_compilers::artifacts::ConfigurableContractArtifact;
use foundry_compilers::resolver::parse::SolData;
use foundry_compilers::ProjectPathsConfig;
use revm_inspectors::tracing::types::CallTraceStep;
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::fs;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Deserialize)]
struct DebuggerContext {
    pub debug_arena: Vec<DebugNode>,
    pub contracts: ContractsDump,
}

#[derive(Deserialize)]
pub struct DebugNode {
    pub kind: String,
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

    for (_file_path, artifact) in artifacts {
        if let Some(ast) = artifact.ast.clone() {
            let absolute_path = ast.absolute_path;

            ast.nodes.iter().for_each(|node| {
                let node = node.clone();

                if let Some(deployed_bytecode) = artifact.deployed_bytecode.as_ref() {
                    let deployed_bytecode = deployed_bytecode.bytecode.as_ref().unwrap().clone();
                    let bytecode = artifact.bytecode.as_ref().unwrap().clone();

                    if node.node_type == NodeType::ContractDefinition {
                        let contract_name = node.attribute::<String>("name").unwrap();

                        let analyze_contract = match contracts_involved {
                            Some(contracts_involved) => contracts_involved.contains(&contract_name),
                            None => true,
                        };

                        if analyze_contract {
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
                            debug_unit.insert(contract_name, visitor.debug_unit);
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

#[derive(Debug, Clone, Default)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
    pub end_line: Option<usize>,
    pub end_column: Option<usize>,
    pub start_offset: usize,
    pub length: Option<usize>,
}

struct StatementVisitor {
    pub source: String,
    pub debug_unit: DebugUnit,

    pub source_map: SourceMap,
    pub pc_ic_map: IcPcMap,

    // this are the ones used for deployment
    pub deployed_source_map: SourceMap,
    pub deployed_pc_ic_map: IcPcMap,

    // in_constructor signals if we are in the constructor of the contract
    pub in_constructor: bool,

    // reference to the contract node that we are visiting
    pub contract_node: Option<Node>,
}

impl StatementVisitor {
    pub fn new(
        deployed_bytecode: CompactBytecode,
        bytecode: CompactBytecode,
        path: String,
        source: String,
    ) -> Self {
        Self {
            source_map: parse(&deployed_bytecode.clone().source_map.unwrap()).unwrap(),
            pc_ic_map: IcPcMap::new(&deployed_bytecode.bytes().unwrap()),
            source: source.clone(),
            deployed_source_map: parse(&bytecode.clone().source_map.unwrap()).unwrap(),
            deployed_pc_ic_map: IcPcMap::new(&bytecode.bytes().unwrap()),
            in_constructor: false,
            contract_node: None,
            debug_unit: DebugUnit {
                name: String::new(),
                path: path.clone(),
                functions: HashMap::new(),
                variables: Vec::new(),
            },
        }
    }

    pub fn visit_contract(&mut self, node: &Node) -> eyre::Result<()> {
        self.contract_node = Some(node.clone());

        for node in &node.nodes {
            match node.node_type {
                NodeType::FunctionDefinition => {
                    let function = self.build_debug_function(node)?;
                    self.debug_unit
                        .functions
                        .insert(function.name.clone(), function);
                }
                NodeType::VariableDeclaration => {
                    let state_variable =
                        node.attribute::<bool>("stateVariable").unwrap_or_default();

                    if state_variable {
                        let var = self.build_debug_variable(node)?;
                        self.debug_unit.variables.push(var.unwrap());
                    }
                }
                NodeType::StructDefinition => {}
                NodeType::EventDefinition => {}
                _ => {
                    panic!("Not handled {:?}", node.node_type);
                }
            }
        }
        Ok(())
    }

    fn source_location2_for(&self, loc: &ast::LowFidelitySourceLocation) -> SourceLocation {
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

    fn find_exit_pc(&self, node: &Node) -> eyre::Result<Option<usize>> {
        let (pc_ic_map, source_map) = if self.in_constructor {
            (&self.deployed_pc_ic_map, &self.deployed_source_map)
        } else {
            (&self.pc_ic_map, &self.source_map)
        };

        // Find source elements within function's range that have Jump::Out
        let start = node.src.start as u32;
        let length = node.src.length.unwrap_or(0) as u32;

        let matches = source_map
            .iter()
            .enumerate()
            .filter(|(_, elem)| {
                if self.in_constructor {
                    // constructor does not have a jump out, mabye because it does not have a switch
                    elem.offset() == start && elem.length() == length
                } else {
                    elem.offset() == start && elem.length() == length && elem.jump() == Jump::Out
                }
            })
            .collect::<Vec<_>>();

        // Get the PC for the last matching element (if any)
        if let Some((idx, _)) = matches.last() {
            Ok(pc_ic_map.get(*idx))
        } else {
            Ok(None)
        }
    }

    fn build_debug_function(&mut self, node: &Node) -> eyre::Result<Function> {
        let is_constructor = node.attribute::<String>("kind").unwrap() == "constructor";
        self.in_constructor = is_constructor;

        let name = node.attribute("name").unwrap();
        let parameters = self.build_debug_parameters(node)?;

        let root_block = if let Some(body) = &node.body {
            self.build_debug_block(body)?
        } else {
            Block::default()
        };

        let exit_pc = if is_constructor {
            // the constructor does not have an ouput but we can use the refercence to the contract node for this
            self.find_exit_pc(self.contract_node.as_ref().unwrap())
                .unwrap()
                .unwrap()
        } else {
            self.find_exit_pc(node).unwrap().unwrap()
        };

        let function = Function {
            name,
            entry_pc: self.get_entry_pc_for_function(node).unwrap(),
            root_block,
            parameters,
            exit_pc: exit_pc,
        };

        self.in_constructor = false;
        Ok(function)
    }

    fn get_entry_pc_for_function(&self, node: &Node) -> Option<usize> {
        let (pc_ic_map, source_map) = if self.in_constructor {
            (&self.deployed_pc_ic_map, &self.deployed_source_map)
        } else {
            (&self.pc_ic_map, &self.source_map)
        };

        let start = node.src.start as u32;
        let length = node.src.length? as u32;

        // Get all matching source elements for this node
        let matches: Vec<_> = source_map
            .iter()
            .enumerate()
            .filter(|(_, elem)| elem.offset() == start && elem.length() == length)
            .collect();

        // Find the match that comes after a JUMP OUT instruction
        for i in 0..matches.len() {
            let (idx, _) = matches[i];

            // Check if there's a previous instruction with JUMP OUT
            if idx > 0 {
                let prev_elem = &source_map[idx - 1];
                if prev_elem.jump() == Jump::Out {
                    // Return the PC for this match
                    return pc_ic_map.get(idx);
                }
            }
        }

        // If no match after JUMP OUT found, return the first match's PC
        matches.first().and_then(|(idx, _)| pc_ic_map.get(*idx))
    }

    fn build_debug_block(&mut self, node: &Node) -> eyre::Result<Block> {
        let mut block = Block {
            variables: Vec::new(),
            condition: None,
            instructions: Vec::new(),
            scopes: Vec::new(),
            location: self.source_location2_for(&node.src),
        };

        let statements: Vec<Node> = node.attribute("statements").unwrap_or_default();
        for statement in &statements {
            match statement.node_type {
                NodeType::ExpressionStatement => {
                    // Add regular statement instruction
                    if let Some(pc) = self.get_pc_for_node(statement) {
                        let block_location = self.source_location2_for(&statement.src);

                        block.instructions.push(Instruction {
                            pc,
                            location: block_location.clone(),
                            kind: InstructionKind::Statement,
                        });

                        // Process for function calls within the expression
                        if let Some(expr) = statement.attribute::<Node>("expression") {
                            self.process_expression_for_function_calls(
                                &expr,
                                &mut block,
                                &block_location,
                            )?;
                        }
                    }
                }
                NodeType::IfStatement => {
                    // Create single block for if statement body
                    if let Some(true_body) = statement.attribute("trueBody") {
                        let mut if_block = self.build_debug_block(&true_body)?;
                        // Add the condition to the block
                        if let Some(condition) = statement.attribute("condition") {
                            if let Some(pc) = self.get_pc_for_node(&condition) {
                                if_block.condition = Some(Instruction {
                                    pc,
                                    location: self.source_location2_for(&condition.src),
                                    kind: InstructionKind::Statement,
                                });
                            }
                        }
                        block.scopes.push(if_block);
                    }
                }
                NodeType::ForStatement => {
                    // Create single block for for loop body
                    if let Some(body) = &statement.body {
                        let mut for_block = self.build_debug_block(body)?;
                        // Add the condition to the block
                        if let Some(condition) = statement.attribute("condition") {
                            if let Some(pc) = self.get_pc_for_node(&condition) {
                                for_block.condition = Some(Instruction {
                                    pc,
                                    location: self.source_location2_for(&condition.src),
                                    kind: InstructionKind::Statement,
                                });
                            }
                        }
                        block.scopes.push(for_block);
                    }
                }
                NodeType::VariableDeclarationStatement => {
                    let var = self.build_debug_variable(statement).unwrap().unwrap();
                    self.debug_unit.variables.push(var.clone());
                    block.variables.push(var.id as usize);

                    let block_location = self.source_location2_for(&statement.src);

                    if let Some(pc) = self.get_pc_for_node(statement) {
                        block.instructions.push(Instruction {
                            pc,
                            location: block_location.clone(),
                            kind: InstructionKind::VariableDeclaration(var.id as usize),
                        });
                    }

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
                    if let Some(pc) = self.get_pc_for_node(statement) {
                        block.instructions.push(Instruction {
                            pc,
                            location: self.source_location2_for(&statement.src),
                            kind: InstructionKind::Statement,
                        });
                    }
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
    ) -> eyre::Result<()> {
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
                if let Some(pc) = self.get_first_pc_for_node(node) {
                    block.instructions.push(Instruction {
                        pc,
                        location: block_location.clone(),
                        kind: InstructionKind::FunctionCall,
                    });
                }

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

    fn get_pc_for_node(&self, node: &Node) -> Option<usize> {
        let (pc_ic_map, source_map) = if self.in_constructor {
            (&self.deployed_pc_ic_map, &self.deployed_source_map)
        } else {
            (&self.pc_ic_map, &self.source_map)
        };

        let start = node.src.start as u32;
        let length = node.src.length? as u32;

        source_map
            .iter()
            .enumerate()
            .filter(|(_, elem)| elem.offset() == start && elem.length() == length)
            .last()
            .and_then(|(idx, _)| pc_ic_map.get(idx))
    }

    fn get_first_pc_for_node(&self, node: &Node) -> Option<usize> {
        let (pc_ic_map, source_map) = if self.in_constructor {
            (&self.deployed_pc_ic_map, &self.deployed_source_map)
        } else {
            (&self.pc_ic_map, &self.source_map)
        };

        let start = node.src.start as u32;
        let length = node.src.length? as u32;

        source_map
            .iter()
            .enumerate()
            .filter(|(_, elem)| elem.offset() == start && elem.length() == length)
            .next()
            .and_then(|(idx, _)| pc_ic_map.get(idx))
    }

    fn build_debug_variable(&self, node: &Node) -> eyre::Result<Option<Variable>> {
        if let Some(name) = node.attribute("name") {
            // this is most likely a state variable
            if let Some(id) = node.id {
                return Ok(Some(Variable {
                    name,
                    id: id as u64,
                    location: self.source_location2_for(&node.src),
                    state_variable: true,
                }));
            }
        } else {
            // check now for a normal varaible decalration
            let declarations = node.attribute::<Vec<Node>>("declarations").unwrap();
            let declaration = declarations.first().unwrap();
            let name = declaration.attribute::<String>("name").unwrap();

            return Ok(Some(Variable {
                name,
                id: node.id.unwrap() as u64,
                location: self.source_location2_for(&node.src),
                state_variable: false,
            }));
        }
        Ok(None)
    }

    fn build_debug_parameters(&self, node: &Node) -> eyre::Result<Vec<Variable>> {
        let mut parameters = Vec::new();
        if let Some(params) = node.attribute::<Vec<Node>>("parameters") {
            for param in params {
                if let Some(var) = self.build_debug_variable(&param)? {
                    parameters.push(var);
                }
            }
        }
        Ok(parameters)
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
        if current_byte >= 0x60 && current_byte <= 0x7f {
            // Calculate push size: for PUSH1 (0x60) it's 1, for PUSH32 (0x7f) it's 32
            let push_size = (current_byte - 0x60 + 1) as usize;
            pc += push_size;
            cumulative_push_size += push_size;
        }

        pc += 1;
    }
    map
}

// Conversion from your existing structures
impl DebugUnit {
    /// Get the source location for a given PC
    pub fn get_location_at_pc(&self, pc: usize) -> (Option<&Instruction>, Vec<usize>) {
        self.functions
            .values()
            .find_map(|function| {
                // Helper function to recursively search blocks
                fn search_block(
                    block: &Block,
                    pc: usize,
                    parent_vars: Vec<usize>,
                ) -> (Option<&Instruction>, Vec<usize>) {
                    let mut vars_in_scope = parent_vars.clone();

                    // Check instructions in current block
                    for inst in &block.instructions {
                        match inst.kind {
                            InstructionKind::VariableDeclaration(id) => {
                                vars_in_scope.push(id);
                            }
                            _ => {}
                        };

                        if inst.pc == pc {
                            return (Some(inst), vars_in_scope);
                        }
                    }

                    // Recursively check nested scopes
                    for scope in &block.scopes {
                        let (found, vars) = search_block(scope, pc, vars_in_scope.clone());
                        if found.is_some() {
                            return (found, vars);
                        }
                    }

                    // Check condition
                    if let Some(ref cond) = block.condition {
                        if cond.pc == pc {
                            return (Some(cond), vars_in_scope);
                        }
                    }
                    (None, vars_in_scope)
                }

                // Initialize vars_in_scope with state variable IDs
                let vars_in_scope = self
                    .variables
                    .iter()
                    .filter(|v| v.state_variable)
                    .map(|v| v.id as usize)
                    .collect();
                let result = search_block(&function.root_block, pc, vars_in_scope);
                if result.0.is_some() {
                    Some(result)
                } else {
                    None
                }
            })
            .unwrap_or((None, vec![]))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepKind {
    FunctionDefinition(String),
    FunctionCall,
    Statement,
}

#[derive(Debug, Clone)]
pub struct DebugTrace {
    pub steps: Vec<DebugStep>,
    pub variables: HashMap<u64, Variable>,
    pub indx: usize,
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
    pub fn prev(&mut self) -> bool {
        while self.indx > 0 {
            self.indx -= 1;
            if !matches!(self.steps[self.indx].kind, StepKind::FunctionDefinition(_)) {
                return true;
            }
        }
        false
    }

    pub fn next(&mut self) -> bool {
        while self.indx < self.steps.len() - 1 {
            self.indx += 1;
            if !matches!(self.steps[self.indx].kind, StepKind::FunctionDefinition(_)) {
                return true;
            }
        }
        false
    }

    pub fn trace(&self) -> DebugTraceStep {
        let mut call_trace = Vec::new();
        let step = self.steps.get(self.indx).unwrap();

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

    pub fn scope(&self) -> Vec<Variable> {
        // so the frame id is the id in the call trace
        // for now lets keep it simple and assume it is the current step
        let step = self.steps.get(self.indx).unwrap();
        step.variables_in_scope
            .iter()
            .map(|id| self.variables.get(&(*id as u64)).unwrap().clone())
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct DebugStep {
    pub location: SourceLocation,
    pub variables_in_scope: Vec<usize>,
    pub path: String,
    pub call_trace: Vec<usize>,
    pub kind: StepKind,
    pub memory: Bytes,
    pub stack: Vec<Bytes>,
}

impl From<&DebugStep> for StackFrame {
    fn from(step: &DebugStep) -> Self {
        StackFrame {
            location: step.location.clone(),
            path: step.path.clone(),
        }
    }
}

pub fn example1(workspace_path: &str, trace_path: &str) -> eyre::Result<DebugTrace> {
    let content = fs::read_to_string(trace_path)
        .map_err(|e| eyre::eyre!("Failed to read debug dump file: {}", e))?;

    let context: DebuggerContext = serde_json::from_str(&content)
        .map_err(|e| eyre::eyre!("Failed to parse debug dump JSON: {}", e))?;

    let contracts_involved: HashSet<String> = context
        .contracts
        .identified_contracts
        .values()
        .cloned()
        .collect();

    let root_path = Path::new(workspace_path);
    let debug_units = generate_debug_units(root_path, Some(&contracts_involved)).unwrap();

    let mut steps: Vec<DebugStep> = Vec::new();

    let mut call_trace = Vec::new();
    let mut expecting_function = true;

    for node in context.debug_arena.iter() {
        for step in node.steps.iter() {
            let memory = Bytes::from(step.memory.clone().unwrap().as_bytes().to_vec());
            let stack: Vec<Bytes> = step
                .stack
                .clone()
                .unwrap()
                .iter()
                .map(|b| Bytes::from(b.as_le_bytes().to_vec()))
                .collect();

            if let Some(contract_name) = context.contracts.identified_contracts.get(&step.contract)
            {
                if let Some(debug_unit) = debug_units.get(contract_name) {
                    // for all the functions, find the entry pc
                    for func in debug_unit.functions.values() {
                        if func.entry_pc == step.pc {
                            if !expecting_function {
                                panic!("Found function entry without call");
                            }
                            expecting_function = false;

                            let is_first_step = steps.len() == 0;

                            steps.push(DebugStep {
                                location: func.root_block.location.clone(),
                                path: debug_unit.path.clone(),
                                memory: memory.clone(),
                                stack: stack.clone(),
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
                        if func.exit_pc == step.pc {
                            if expecting_function {
                                panic!("Found function exit without call");
                            }

                            // pop the last call trace and make sure the function is the same
                            let last_call = call_trace.pop();
                            if last_call.is_some() && last_call.unwrap().0.name.clone() != func.name
                            {
                                panic!("function {:?} has exit pc {:?} but the last call trace is {:?}", func.name, func.exit_pc, last_call.unwrap().0.name);
                            }
                        }
                    }

                    if let (Some(instruction), variables_in_scope) =
                        debug_unit.get_location_at_pc(step.pc)
                    {
                        // get a list of all the calltrace ids
                        if expecting_function {
                            panic!("Found function call without function entry");
                        }

                        if matches!(instruction.kind, InstructionKind::FunctionCall) {
                            expecting_function = true;

                            steps.push(DebugStep {
                                location: instruction.location.clone(),
                                path: debug_unit.path.clone(),
                                variables_in_scope,
                                memory: memory.clone(),
                                stack: stack,
                                call_trace: call_trace.iter().map(|(_, pos)| *pos).collect(),
                                kind: StepKind::FunctionCall,
                            });
                        } else {
                            steps.push(DebugStep {
                                location: instruction.location.clone(),
                                path: debug_unit.path.clone(),
                                variables_in_scope,
                                memory: memory.clone(),
                                stack: stack,
                                call_trace: call_trace.iter().map(|(_, pos)| *pos).collect(),
                                kind: StepKind::Statement,
                            });
                        }
                    }
                }
            }
        }
    }

    // the last step should have call trace equal to 0
    if steps.last().unwrap().call_trace.len() != 0 {
        panic!("Last step should have call trace equal to 0");
    }

    // loop over all the debug units and get the variable definitions
    let mut variable_definitions = HashMap::new();
    for debug_unit in debug_units.values() {
        for variable in debug_unit.variables.iter() {
            variable_definitions.insert(variable.id, variable.clone());
        }
    }

    Ok(DebugTrace {
        steps,
        indx: 0,
        variables: variable_definitions,
    })
}

#[derive(Debug, Clone)]
pub struct DebugUnit {
    pub name: String,
    pub path: String,
    pub functions: HashMap<String, Function>,
    pub variables: Vec<Variable>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub entry_pc: usize,
    pub exit_pc: usize,
    pub root_block: Block,
    pub parameters: Vec<Variable>,
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

#[derive(Debug, Clone)]
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
    pub pc: usize,
    pub location: SourceLocation,
    pub kind: InstructionKind,
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

        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "Forge command failed: {} {} {}",
                workspace_path, stdout, stderr
            ),
        ))
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
            .arg("false");

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
                write!(f, "    (params )\n")?;
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
                    StepKind::FunctionCall => {
                        writeln!(
                            output,
                            "{}[CALL] {}:{}",
                            indent, step.location.line, step.location.column
                        )
                        .unwrap();
                    }
                    StepKind::Statement => {
                        // Get names of variables in scope
                        let vars_in_scope: Vec<String> = step
                            .variables_in_scope
                            .iter()
                            .filter_map(|&id| self.variables.get(&(id as u64)))
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

                let deployed_bytecode = artifact.compact_deployed_bytecode();
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

    #[test]
    fn test_debugger_traces() {
        let workspace_path_string = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR not set")
            + "/src/testcases";

        let workspace_path = workspace_path_string.as_str();
        let test_dir = Path::new(workspace_path).join("test");

        for entry in fs::read_dir(test_dir).unwrap() {
            let path = entry.unwrap().path();

            if path.is_file() && path.extension().unwrap() == "sol" {
                // the trace file is the name of the file with .trace extension
                let expected_path = path.with_extension("trace");

                // use as test path for the output command the relative path with respect to the workspace path
                let test_path = path.strip_prefix(workspace_path).unwrap();
                let debug_trace_path = "/tmp/debug_trace.json";

                let forge =
                    Forge::debug("test_main", test_path.to_str().unwrap(), debug_trace_path);
                let _ = execute_command(workspace_path, forge).unwrap();

                let debug_trace = example1(workspace_path, debug_trace_path).unwrap();

                let formatted = debug_trace.to_debug_format(workspace_path);
                let expected = fs::read_to_string(expected_path).unwrap();

                assert_eq!(formatted, expected);
            }
        }
    }
}
