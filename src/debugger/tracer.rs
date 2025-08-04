use crate::debugger::metrics::{Action, MetricsRecorder};
use crate::debugger::state::{
    parse_variable_declaration_type, StateReference, StoragePosition, Type,
};
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use dashmap::mapref::one::Ref;
use dashmap::DashMap;
use foundry_compilers::artifacts::ast::{self, Node, NodeType};
use foundry_compilers::artifacts::sourcemap::Jump;
use foundry_compilers::artifacts::sourcemap::SourceElement;
use foundry_compilers::artifacts::sourcemap::{parse, SourceMap};
use foundry_compilers::artifacts::CompactBytecode;
use foundry_compilers::artifacts::ConfigurableContractArtifact;
use foundry_compilers::cache::CompilerCache;
use foundry_compilers::resolver::parse::SolData;
use foundry_compilers::solc::SolcSettings;
use foundry_compilers::ProjectPathsConfig;
use rayon::prelude::*;
use revm_inspectors::tracing::types::CallTraceStep;
use serde::Deserialize;
use serde::Serialize;
use slice_group_by::GroupBy;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct TraceContext {
    contract_state_variables: HashMap<usize, Vec<usize>>,
    structs: HashMap<usize, Node>,
    state_variables: HashMap<usize, Variable>,
}

impl TraceContext {
    fn new() -> Self {
        Self {
            contract_state_variables: HashMap::new(),
            structs: HashMap::new(),
            state_variables: HashMap::new(),
        }
    }

    pub fn merge(&mut self, other: TraceContext) {
        self.structs.extend(other.structs);
        self.state_variables.extend(other.state_variables);
        self.contract_state_variables
            .extend(other.contract_state_variables);
    }
}

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
    pub sources: Sources,
}

#[derive(Deserialize)]
pub struct Sources {
    pub sources_by_id: HashMap<String, HashMap<u32, Source>>,
    pub artifacts_by_name: HashMap<String, Vec<DebugNodeArtifact>>,
}

#[derive(Deserialize)]
pub struct DebugNodeArtifact {
    pub file_id: u32,
}

#[derive(Deserialize)]
pub struct Source {
    pub path: PathBuf,
}

fn load_artifact(
    file_path: &Path,
) -> Result<ConfigurableContractArtifact, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(file_path)?;
    let artifact = serde_json::from_str::<ConfigurableContractArtifact>(&content)?;

    Ok(artifact)
}

fn retrieve_types(absolute_path: &Path) -> eyre::Result<TraceContext> {
    let mut trace_context = TraceContext::new();

    let artifact = load_artifact(absolute_path).map_err(|e| {
        tracing::error!("error loading artifact {:?} {:?}", absolute_path, e);
        eyre::eyre!("Failed to load artifact: {}", e)
    })?;

    // insert the contract as well because it can be referenced by other contracts
    let Some(ast) = artifact.ast else {
        tracing::warn!("No AST found in artifact for path: {:?}", absolute_path);
        return Ok(trace_context);
    };

    // For all the contracts parse and extract struct references into TraceContext
    // TODO: Merge this with the contract visitor.
    ast.nodes.iter().for_each(|node| {
        if node.node_type == NodeType::ContractDefinition {
            let contract_node = node.clone();
            let mut contract_state_variables = Vec::new();

            // insert the contract as well because it can be referenced by other contracts
            // in variable declarations
            trace_context
                .structs
                .insert(contract_node.id.unwrap(), contract_node.clone());

            contract_node.nodes.iter().for_each(|node| {
                let node_id = node.id.unwrap();

                if node.node_type == NodeType::StructDefinition
                    || node.node_type == NodeType::UserDefinedValueTypeDefinition
                    || node.node_type == NodeType::EnumDefinition
                {
                    trace_context.structs.insert(node_id, node.clone());
                } else if node.node_type == NodeType::VariableDeclaration {
                    // not sure if I have to filter by stateVariable here
                    let variable =
                        StatementVisitor::build_debug_variable(node, VariableLocation::Storage)
                            .expect("variable")
                            .unwrap();

                    trace_context.state_variables.insert(node_id, variable);
                    contract_state_variables.push(node_id);
                }
            });

            trace_context
                .contract_state_variables
                .insert(contract_node.id.unwrap(), contract_state_variables);
        } else if node.node_type == NodeType::StructDefinition
            || node.node_type == NodeType::UserDefinedValueTypeDefinition
            || node.node_type == NodeType::EnumDefinition
        {
            let node_id = node.id.unwrap();
            trace_context.structs.insert(node_id, node.clone());
        }
    });

    Ok(trace_context)
}

fn generate_debug_units(root_path: &Path) -> Result<TraceContext, Box<dyn std::error::Error>> {
    tracing::info!("Generating debug units...");
    let config: ProjectPathsConfig<SolData> =
        ProjectPathsConfig::dapptools(Path::new(root_path)).unwrap();

    let cache = CompilerCache::<SolcSettings>::read(&config.cache).map_err(|e| {
        // TODO: Add custom error enum
        tracing::error!("error reading cache {:?}", e);
        e
    })?;

    tracing::info!("Found {} cache entries", cache.len());

    let ast_to_parse = cache
        .entries()
        .flat_map(|cache_entry| {
            tracing::info!("Processing file: {:?}", cache_entry.source_name);

            // we only want one of the contract artifact since each one contains the whole AST
            // object for the file and since we want to do a full AST traversal we do not care which one we pick
            let cached_artifact = cache_entry
                .artifacts()
                .next()
                .expect("expected at least one artifact");

            let absolute_path = config.artifacts.join(cached_artifact.path.clone());
            if !absolute_path.exists() {
                // it could be that the artifact does not exists yet, Forge would put values in the cache that
                // are not in the out directory. For example, if you have independent tests (A, B) and debug test A
                // test B will show up in the cache directory but it will not have an artifact.
                None
            } else {
                Some(absolute_path)
            }
        })
        .collect::<Vec<_>>();

    let results: Vec<TraceContext> = ast_to_parse
        .par_iter()
        .filter_map(|absolute_path| {
            match retrieve_types(absolute_path) {
                // Much simpler call
                Ok(context) => Some(context),
                Err(e) => {
                    tracing::error!(
                        "Failed to retrieve types for artifact {:?}: {:?}",
                        absolute_path,
                        e
                    );
                    None
                }
            }
        })
        .collect();

    let mut final_trace_context = TraceContext::new();
    for context in results {
        final_trace_context.merge(context);
    }

    Ok(final_trace_context)
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
    UnexpectedStorageLocation(String),
}

impl Display for StatementVisitorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatementVisitorError::ParseError => write!(f, "ParseError"),
            StatementVisitorError::MissingAttribute(attribute_name) => {
                write!(f, "MissingAttribute: {attribute_name}")
            }
            #[allow(clippy::uninlined_format_args)]
            StatementVisitorError::IncorrectType(expected, actual) => write!(
                f,
                "IncorrectType: expected {:?}, got {:?}",
                expected, actual
            ),
            StatementVisitorError::UnexpectedStorageLocation(loc) => {
                write!(f, "UnexpectedStorageLocation: {loc}")
            }
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
        contract_name: String,
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
                contract_name,
                source_id: 0,
                location: Default::default(),
                path: path.clone(),
                functions: HashMap::new(),
                variables: Vec::new(),
                bytecode: BytecodeMap::new(&bytecode),
                deployed_bytecode: BytecodeMap::new(&deployed_bytecode),
                state_variables: Vec::new(),
                linearized_base_contracts: Vec::new(),
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
                    /*
                    let state_variable = node
                        .attribute::<bool>("stateVariable")
                        .ok_or_missing_attribute("stateVariable")?;

                    if state_variable {
                        let var = Self::build_debug_variable(node)?;
                        self.debug_unit.variables.push(var.unwrap());
                    }
                    */
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
                NodeType::EnumDefinition
                | NodeType::UsingForDirective
                | NodeType::UserDefinedValueTypeDefinition
                | NodeType::ErrorDefinition => {
                    // TODO
                }
                _ => {
                    // Panic here because I want to be aware of the missing types
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

    fn flatten_if_else_chain(&mut self, if_node: &Node) -> StatementVisitorResult<Vec<Block>> {
        let mut blocks = Vec::new();
        let mut current_node = if_node.clone(); // Clone the initial node

        loop {
            // Process the current if/else-if
            if let Some(true_body) = current_node.attribute::<Node>("trueBody") {
                let mut if_block = self.build_debug_block(&true_body)?;

                // Add the condition to this block
                if let Some(condition) = current_node.attribute::<Node>("condition") {
                    if_block.condition = Some(Instruction {
                        location: self.source_location_for(&condition.src),
                        kind: InstructionKind::Statement,
                        loc: SourceLocationHelper {
                            start: condition.src.start,
                            length: condition.src.length.unwrap(),
                        },
                    });
                }

                blocks.push(if_block);
            }

            // Check for false body
            if let Some(false_body) = current_node.attribute::<Node>("falseBody") {
                if false_body.node_type == NodeType::IfStatement {
                    // Clone the else-if node for next iteration
                    current_node = false_body;
                } else {
                    // This is a final else block
                    let else_block = self.build_debug_block(&false_body)?;
                    blocks.push(else_block);
                    break;
                }
            } else {
                // No else clause, we're done
                break;
            }
        }

        Ok(blocks)
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
                    let flatten_blocks = self.flatten_if_else_chain(statement)?;
                    block.scopes.extend(flatten_blocks);
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
                    let var = if let Some(var) =
                        Self::build_debug_variable(statement, VariableLocation::Stack)?
                    {
                        var
                    } else {
                        continue;
                    };

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
                NodeType::TryStatement => {
                    let clauses = statement
                        .attribute::<Vec<Node>>("clauses")
                        .unwrap_or_default();

                    let external_call = statement
                        .attribute::<Node>("externalCall")
                        .ok_or_missing_attribute("externalCall")?;

                    let block_location = self.source_location_for(&statement.src);

                    self.process_expression_for_function_calls(
                        &external_call,
                        &mut block,
                        &block_location,
                    )?;

                    let mut catch_blocks = Vec::new();
                    for clause in clauses {
                        let block = clause
                            .attribute::<Node>("block")
                            .ok_or_missing_attribute("block")?;

                        let params = if clause.attribute::<Node>("parameters").is_some() {
                            self.build_debug_parameters(&clause)?
                        } else {
                            Vec::new()
                        };
                        let variables = params.iter().map(|var| var.id as usize).collect();

                        let mut block = self.build_debug_block(&block)?;
                        block.variables = variables;

                        catch_blocks.push(block);
                    }

                    block.scopes.push(Block {
                        variables: Vec::new(),
                        condition: None,
                        instructions: Vec::new(),
                        scopes: catch_blocks,
                        location: block_location,
                    });
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

    #[allow(clippy::only_used_in_recursion)]
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

    fn build_debug_variable(
        node: &Node,
        default_location: VariableLocation,
    ) -> StatementVisitorResult<Option<Variable>> {
        if let Some(name) = node.attribute("name") {
            // Code path for state variables and function parameters
            let type_name = node.attribute::<Node>("typeName").unwrap();
            let is_constant = node.attribute::<bool>("constant").unwrap();

            let storage_location = VariableLocation::from_str(
                node.attribute::<String>("storageLocation")
                    .ok_or_missing_attribute("storageLocation")?
                    .as_str(),
                default_location,
            )?;

            let state_variable = storage_location == VariableLocation::Storage;

            // this is most likely a state variable
            if let Some(id) = node.id {
                return Ok(Some(Variable {
                    name,
                    id: id as u64,
                    state_variable,
                    location: storage_location,
                    is_constant,
                    type_name,
                }));
            }
        } else {
            // code path for block variables
            let declarations = match node.attribute::<Vec<Node>>("declarations") {
                Some(dec) => dec,
                None => {
                    // TODO: A statement like
                    // (,val,) => call()
                    // geneates a vector with null values which does not get parsed with this declarations statement
                    return Ok(None);
                }
            };

            let declaration = declarations.first().unwrap();

            let name = declaration
                .attribute::<String>("name")
                .ok_or_missing_attribute("name")?;

            let type_name = declaration.attribute::<Node>("typeName").unwrap();
            let is_constant = declaration.attribute::<bool>("constant").unwrap();

            let storage_location = VariableLocation::from_str(
                declaration
                    .attribute::<String>("storageLocation")
                    .ok_or_missing_attribute("storageLocation")?
                    .as_str(),
                default_location,
            )?;

            return Ok(Some(Variable {
                name,
                id: node.id.unwrap() as u64,
                state_variable: false,
                location: storage_location,
                is_constant,
                type_name,
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

            if let Some(var) = Self::build_debug_variable(&param, VariableLocation::Stack)? {
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    Function(Box<Function>),
    Instruction(Instruction),
}

impl MatchResult {
    #[allow(dead_code)]
    fn type_string(&self) -> String {
        match self {
            MatchResult::Function(func) => format!("Function: {}", func.name),
            MatchResult::Instruction(_) => "Instruction".to_string(),
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

            if loc.offset() == 419 {
                println!("=> Scopes: {:?}", block.scopes);
            }

            // Check the condition statement first before we start to accumulate the vars_in_scope
            // The condition statement does not have in scope any of the variables defined in the block
            if let Some(cond) = &block.condition {
                if cond.loc.matches(loc) {
                    return (Some(MatchResult::Instruction(cond.clone())), vars_in_scope);
                }
            }

            for inst in &block.instructions {
                if let InstructionKind::VariableDeclaration(id) = inst.kind {
                    vars_in_scope.push(id);
                };

                if inst.loc.matches(loc) {
                    return (Some(MatchResult::Instruction(inst.clone())), vars_in_scope);
                }
            }

            for scope in &block.scopes {
                if let (Some(result), vars) = search_block(scope, loc, vars_in_scope.clone()) {
                    return (Some(result), vars);
                }
            }

            (None, vec![])
        }

        // try to find exact match by brute force for now
        for func in self.functions.values() {
            // Initialize vars_in_scope with state variable IDs
            let mut vars_in_scope: Vec<usize> = vec![];

            // we track other state variables in this other place
            // TODO: deprecate the previous one. I am still unsure if we still need it, I will keep it
            // until there are more unit tests.
            vars_in_scope.extend(self.state_variables.iter().copied());

            // add variables from the function parameters
            for func in func.parameters.iter() {
                vars_in_scope.push(func.id as usize);
            }

            if func.loc.matches(loc) {
                return (
                    Some(MatchResult::Function(Box::new(func.clone()))),
                    vars_in_scope,
                );
            }

            if let (Some(result), vars) = search_block(&func.root_block, loc, vars_in_scope) {
                return (Some(result), vars);
            }
        }

        (None, vec![])
    }
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub enum FunctionCallKind {
    Call,
    Create,
}

impl std::fmt::Display for FunctionCallKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FunctionCallKind::Call => write!(f, "CALL"),
            FunctionCallKind::Create => write!(f, "CREATE"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub enum StepKind {
    FunctionDefinition(String),
    FunctionCall(FunctionCallKind),
    FunctionExit,
    Statement(bool),

    #[default]
    Unknown,
}

impl StepKind {
    pub fn is_function_call(&self) -> bool {
        matches!(self, StepKind::FunctionCall(_))
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DebugTrace {
    pub steps: Vec<DebugStep>,
    pub variables: HashMap<u64, Variable>,
    pub variable_types: HashMap<u64, Type>,
    pub assignments: HashMap<u64, Assignment>,
    pub metrics: Vec<(Action, Duration)>,
}

#[derive(Debug, Clone)]
pub struct StackFrame {
    pub parent_func: String,
    pub path: String,
    pub location: SourceLocation,
}

#[derive(Debug, Clone)]
pub struct DebugTraceStep {
    pub stack_frames: Vec<StackFrame>,
}

impl DebugTrace {
    pub fn find_parent_function_name(&self, step_index: usize) -> Option<String> {
        // from a given step index, find the parent function name
        // we assume that the parent function is the first step with a FunctionDefinition kind
        // that is before the given step index and in the same call trace level
        let step = self.steps.get(step_index).unwrap();
        let call_trace_len = step.call_trace.len();

        let mut current_index = step_index;
        while current_index > 0 {
            let step = self.steps.get(current_index).unwrap();
            if let StepKind::FunctionDefinition(name) = &step.kind {
                // the call trace only increases on the first statement of the function
                let step_call_trace = self.steps.get(current_index + 1).unwrap();
                if step_call_trace.call_trace.len() == call_trace_len {
                    return Some(name.clone());
                }
            }
            current_index -= 1;
        }
        None
    }

    pub fn trace(&self, indx: usize) -> DebugTraceStep {
        let mut call_trace = Vec::new();
        let step = self.steps.get(indx).unwrap();

        // retrieve the call trace for this step
        for step_trace in step.call_trace.iter() {
            if *step_trace == 0 {
                // TODO note: skip the first step which is always 0
                continue;
            }

            let parent_step = self.steps.get(*step_trace).unwrap();
            assert!(matches!(parent_step.kind, StepKind::FunctionCall(_)));

            let parent_func = self
                .find_parent_function_name(*step_trace)
                .unwrap_or_else(|| "unknown a".to_string());
            call_trace.push(new_stack_frame(parent_step, parent_func));
        }

        // now add the current step to the call trace
        let parent_func = self
            .find_parent_function_name(indx)
            .unwrap_or_else(|| "unknown b".to_string());
        call_trace.push(new_stack_frame(step, parent_func));

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
            .filter_map(|id| match self.variables.get(&(*id as u64)) {
                Some(var) => {
                    if is_forge_variable(var.name.as_str()) {
                        // skip forge variables
                        None
                    } else {
                        Some(var.clone())
                    }
                }
                None => {
                    tracing::error!("variable not found: {}", id);
                    None
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StateSnapshot {
    pub memory: Bytes,
    pub stack: Vec<Bytes>,
    pub storage: HashMap<Bytes, Bytes>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct DebugStep {
    pub location: SourceLocation,
    pub variables_in_scope: Vec<usize>,
    pub path: String,
    pub call_trace: Vec<usize>,
    pub kind: StepKind,
    pub state_snapshot: StateSnapshot,
}

fn new_stack_frame(step: &DebugStep, parent_func: String) -> StackFrame {
    StackFrame {
        parent_func,
        location: step.location.clone(),
        path: step.path.clone(),
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

    #[allow(dead_code)]
    #[error("Found instruction without function entry")]
    FoundInstructionWithoutFunctionEntry,

    #[allow(dead_code)]
    #[error("Last step should have call trace equal to 0")]
    LastStepShouldHaveCallTraceEqualZero,
}

fn source_element_matches(a: &SourceElement, b: &SourceElement) -> bool {
    a.offset() == b.offset() && a.length() == b.length() && a.index_i32() == b.index_i32()
}

pub const REVERT: u8 = 0xfd;
pub const CREATE: u8 = 0xf0;
pub const CALL: u8 = 0xf1;

fn is_call_op_code(op: u8) -> bool {
    op == CALL || op == CREATE
}

// Builder for generating a trace
pub struct Builder {
    workspace_path: String,
    context: DebuggerContext,
    cache: CompilerCache<SolcSettings>,
    debug_units: DashMap<u32, Vec<DebugUnit>>,
    trace_context: Option<TraceContext>,
}

impl Builder {
    pub fn new(workspace_path: &str, trace_path: &str) -> Result<Self, TraceError> {
        let content = fs::read_to_string(trace_path)
            .map_err(|e| TraceError::FailedToReadFile(trace_path.to_string(), e))?;
        let context: DebuggerContext = serde_json::from_str(&content)
            .map_err(|e| TraceError::FailedToParseDebugDump(trace_path.to_string(), e))?;

        let config: ProjectPathsConfig<SolData> =
            ProjectPathsConfig::dapptools(Path::new(workspace_path)).unwrap();
        let cache = CompilerCache::<SolcSettings>::read(&config.cache).unwrap();

        Ok(Self {
            workspace_path: workspace_path.to_string(),
            context,
            cache,
            debug_units: DashMap::new(),
            trace_context: None,
        })
    }

    fn find_source_path(&self, source_id: u32) -> Option<PathBuf> {
        for (_, sources) in self.context.contracts.sources.sources_by_id.iter() {
            for (file_source_id, source) in sources.iter() {
                if *file_source_id == source_id {
                    return Some(source.path.clone());
                }
            }
        }
        None
    }

    pub fn get_debug_unit(&self, source_id: u32) -> Option<Ref<u32, Vec<DebugUnit>>> {
        // Generate if not exists
        if !self.debug_units.contains_key(&source_id) {
            self.generate_debug_unit(source_id);
        }

        // Return reference
        self.debug_units.get(&source_id)
    }

    pub fn generate_debug_unit(&self, source_id: u32) {
        let workspace_path = Path::new(&self.workspace_path);

        let config: ProjectPathsConfig<SolData> =
            ProjectPathsConfig::dapptools(workspace_path).unwrap();

        // compute the debug unit
        let path = self.find_source_path(source_id);
        let Some(path) = path else {
            // if is okay if the path is not found
            return;
        };

        // find the cache entry that belongs to this file
        let Some(entry) = self.cache.entry(path.as_path()) else {
            panic!("there has to be something here at this point");
        };

        let source_absolute_path = workspace_path.join(path.clone());
        let source = fs::read_to_string(source_absolute_path.clone()).unwrap();

        let mut debug_unit = vec![];

        let trace_context = self.trace_context.as_ref().unwrap();

        // read the contracts/artifacts from the cache entry since we cannot know to which contract this source_id belongs
        for cached_artifact in entry.artifacts() {
            let artifact_path = config.artifacts.join(&cached_artifact.path);
            let artifact = load_artifact(&artifact_path).unwrap();

            // Extract the compilation target contract name from the artifact path.
            // Each JSON artifact represents one compilation target, but the AST includes
            // all contracts/nodes from the source file plus imported dependencies.
            // The filename tells us which specific contract was the compilation target.
            // IMPORTANT: Bytecode is only available for the compilation target contract.
            // Attempting to decode other ContractDefinitions with a visitor will fail.
            // e.g., "Parent.json" = Parent is the target with bytecode, but AST contains Parent + Child + imports
            let name = artifact_path.file_name().unwrap().to_str().unwrap();
            let name = name
                .split('/')
                .next_back()
                .unwrap_or("")
                .strip_suffix(".json")
                .unwrap_or("");

            // TODO: this can be missing
            let deployed_bytecode = artifact
                .deployed_bytecode
                .as_ref()
                .expect("Deployed bytecode is missing")
                .bytecode
                .as_ref()
                .unwrap()
                .clone();
            let bytecode = artifact.bytecode.as_ref().unwrap().clone();

            let ast = artifact.ast.expect("ast not found");

            ast.nodes
                .iter()
                .filter(|node| node.node_type == NodeType::ContractDefinition)
                .for_each(|node| {
                    let contract_name = node.attribute::<String>("name").unwrap();

                    if name == contract_name {
                        let mut linearized_base_contracts = node
                            .attribute::<Vec<usize>>("linearizedBaseContracts")
                            .unwrap();
                        linearized_base_contracts.reverse();

                        let mut visitor = StatementVisitor::new(
                            contract_name,
                            deployed_bytecode.clone(),
                            bytecode.clone(),
                            source_absolute_path.to_str().unwrap().to_string(),
                            source.clone(),
                        );

                        let contract_loc = visitor.source_location_for(&node.src);
                        visitor.visit_contract(&node.clone()).unwrap();

                        // just so that we can keep the reference around
                        let mut dd = visitor.debug_unit;
                        dd.source_id = artifact.id.unwrap();
                        dd.linearized_base_contracts = linearized_base_contracts;
                        dd.location = contract_loc;

                        // get the state variables from the trace context
                        let state_variables = dd
                            .linearized_base_contracts
                            .iter()
                            .flat_map(|base_contract_id| {
                                trace_context
                                    .contract_state_variables
                                    .get(base_contract_id)
                                    .unwrap()
                            })
                            .copied()
                            .collect::<Vec<_>>();

                        dd.state_variables = state_variables;

                        debug_unit.push(dd);
                    }
                })
        }

        // we have to insert the debug unit into the cache
        self.debug_units.insert(source_id, debug_unit);

        tracing::info!(
            "Reading debug unit for source id: {source_id} at path: {:?}",
            path
        );
    }

    pub fn for_each_debug_unit<F>(&self, mut f: F)
    where
        F: FnMut(u32, &DebugUnit),
    {
        for entry in self.debug_units.iter() {
            let source_id = *entry.key();
            for debug_unit in entry.value().iter() {
                f(source_id, debug_unit);
            }
        }
    }

    pub fn get_debug_unit_from_name(&self, name: &str) -> eyre::Result<DebugUnit> {
        let Some(artifact_vec) = self.context.contracts.sources.artifacts_by_name.get(name) else {
            return Err(eyre::eyre!("Artifact not found"));
        };

        // now we have all the contracts with that name
        // if there is more than one we cannot process this so we bail out for now
        if artifact_vec.len() > 1 {
            return Err(eyre::eyre!(
                "Multiple artifacts found for contract name: {}",
                name
            ));
        }

        let file_id = artifact_vec[0].file_id;

        // since we have already discarded that there is more than one contract with this name
        // there is going to be only one contract in the debug unit that matches this one
        let Some(debug_units) = self.get_debug_unit(file_id) else {
            return Err(eyre::eyre!("Debug unit not found"));
        };

        for debug_unit in debug_units.iter() {
            if debug_unit.contract_name == name {
                return Ok(debug_unit.clone());
            }
        }

        Err(eyre::eyre!("Not found"))
    }

    pub fn generate_trace(&mut self) -> Result<(DebugTrace, TraceContext), TraceError> {
        let mut metrics_recorder = MetricsRecorder::new();

        let root_path = Path::new(&self.workspace_path);
        let trace_context = generate_debug_units(root_path).unwrap();

        self.trace_context = Some(trace_context.clone());

        metrics_recorder.capture(Action::GenerateDebugUnits);

        // map each contract to its current accumulated storage
        let mut contracts_storage = HashMap::new();

        metrics_recorder.capture(Action::PrepareDebugUnits);

        let mut steps = Vec::new();
        let mut call_trace = Vec::new();

        // it starts as true since we are expecting the first step to be a function call
        // since the trace already has the first step as a function call
        let mut expecting_function = true;

        let mut assignments = HashMap::new();

        // Add an initial call trace and step for the first function call
        // Lets make it a bit dummy for now
        call_trace.push(0);
        steps.push(DebugStep {
            kind: StepKind::FunctionCall(FunctionCallKind::Call),
            ..Default::default()
        });

        for node in self.context.debug_arena.iter() {
            // name of the contract in this step
            let contract = self
                .context
                .contracts
                .identified_contracts
                .get(&node.address)
                .unwrap();

            let mut latest_accumulated_storage_index = 0;

            let debug_unit = self.get_debug_unit_from_name(contract).unwrap();
            // go over all the traces and keep the ones that have a source location
            let steps_with_trace = node
                .steps
                .iter()
                .enumerate()
                .filter_map(|(indx, step)| {
                    let pc = step.pc;

                    let bytecode = if node.kind.is_create() {
                        &debug_unit.bytecode
                    } else {
                        &debug_unit.deployed_bytecode
                    };

                    let ic_index = bytecode.ic_pc_map.get(pc).unwrap();
                    let source_location = bytecode.source_map.get(ic_index).unwrap();

                    let Some(source_id) = source_location.index() else {
                        // -1, which means that this is not a source location
                        return None;
                    };

                    let Some(debug_units_to_test) = self.get_debug_unit(source_id) else {
                        // if we do not have a debug unit for this source id, we cannot match it
                        return None;
                    };

                    for debug_unit_to_test in debug_units_to_test.iter() {
                        if let (Some(loc), vars) =
                            debug_unit_to_test.match_location(source_location)
                        {
                            return Some((
                                step,
                                source_location,
                                loc,
                                vars,
                                debug_unit_to_test.path.clone(),
                                indx,
                            ));
                        }
                    }

                    None
                })
                .collect::<Vec<_>>();

            // now group the traces by source location. This is, all the spans that belong to the same source location
            // will be grouped toghether
            let chunks = steps_with_trace
                .linear_group_by(|a, b| source_element_matches(a.1, b.1))
                .collect::<Vec<_>>();

            for chunk in chunks {
                let first_entry = chunk
                    .first()
                    .expect("There has to be at least one entry in the chunk");

                let elem = first_entry.2.clone();
                let vars_in_scope = first_entry.3.clone();
                let path = first_entry.4.clone();

                let memory = Bytes::from(first_entry.0.memory.clone().unwrap().as_bytes().to_vec());
                let stack: Vec<Bytes> = first_entry
                    .0
                    .stack
                    .clone()
                    .unwrap()
                    .iter()
                    .map(|b| Bytes::from(b.to_be_bytes_vec()))
                    .collect();

                // First, accumulate the storage up to this point, since we are storing the state as state_diffs
                // and we have the index of the step, we just add up the new changes to 'contracts_storage' to update
                // the state up to this point.
                for i in latest_accumulated_storage_index..first_entry.5 {
                    if let Some(storage_change) = node.steps[i].storage_change {
                        contracts_storage
                            .entry(node.steps[i].contract)
                            .or_insert(HashMap::new())
                            .insert(
                                Bytes::from(storage_change.key.to_be_bytes_vec()),
                                Bytes::from(storage_change.value.to_be_bytes_vec()),
                            );
                    }
                }
                latest_accumulated_storage_index = first_entry.5;

                let storage = contracts_storage
                    .get(&node.address)
                    .cloned()
                    .unwrap_or_default();

                let state_snapshot = StateSnapshot {
                    memory,
                    stack,
                    storage,
                };

                let local_call_trace = call_trace.clone();

                match elem {
                    MatchResult::Function(func) => {
                        // Process function elements
                        let func_with_out = chunk.iter().any(|i| i.1.jump() == Jump::Out);
                        if func_with_out {
                            // This function chunk includes an outgoing jump, which signals the exit of the function
                            let Some(_) = call_trace.pop() else {
                                return Err(TraceError::FoundFunctionExitWithoutCall);
                            };

                            // Add the function exit step. We do this independent of whether we popped the function or not
                            steps.push(DebugStep {
                                location: func.root_block.location.clone(),
                                path,
                                variables_in_scope: vars_in_scope.clone(),
                                call_trace: local_call_trace,
                                kind: StepKind::FunctionExit,
                                state_snapshot,
                            })
                        } else {
                            // This is the function entry
                            // Validate that we are expecting a function entry
                            if !expecting_function {
                                return Err(TraceError::FoundFunctionEntryWithoutCall);
                            }
                            expecting_function = false;

                            // add the parameters to the assignments table
                            let num_params = func.parameters.len();
                            let stack_len = state_snapshot.stack.len();

                            // The parameters are located in the stack as the last elements in order
                            // in which they are defined in the function. For example, if a function
                            // has two parameters, the first one will be at stack_len - 2 and the
                            // second one at stack_len - 1.
                            for (i, param) in func.parameters.iter().enumerate() {
                                let stack_pos = stack_len - num_params + i;
                                assignments.insert(param.id, Assignment::Stack(stack_pos));
                            }

                            steps.push(DebugStep {
                                location: func.root_block.location.clone(),
                                path,
                                variables_in_scope: vec![],
                                call_trace: local_call_trace,
                                kind: StepKind::FunctionDefinition(func.name.clone()),
                                state_snapshot,
                            });
                        }
                    }
                    MatchResult::Instruction(inst) => {
                        let last_entry = chunk.last().unwrap();

                        // Process instruction elements
                        if matches!(inst.kind, InstructionKind::FunctionCall) {
                            // There are two signals that we are in a function call or create call, either the last entry is a jump in
                            // or the last opcode is a CALL/CREATE opcode family.
                            // Other combinations of this that might appear are, groups of call backs witht he same source location but withotu any jump in
                            // which is the return of an internal function
                            // or groups of chunks with one internal jump in that is the return of an external function call.
                            if !is_call_op_code(last_entry.0.op.get())
                                && last_entry.1.jump() != Jump::In
                            {
                                continue;
                            }
                        }

                        let stmt_kind = match inst.kind {
                            InstructionKind::FunctionCall => {
                                let is_create = last_entry.0.op.get() == CREATE;
                                if is_create {
                                    StepKind::FunctionCall(FunctionCallKind::Create)
                                } else {
                                    StepKind::FunctionCall(FunctionCallKind::Call)
                                }
                            }
                            _ => {
                                let is_revert = chunk.last().unwrap().0.op.get() == REVERT;
                                StepKind::Statement(is_revert)
                            }
                        };

                        if let InstructionKind::VariableDeclaration(id) = &inst.kind {
                            let var_id = *id as u64;
                            assignments
                                .insert(var_id, Assignment::Stack(state_snapshot.stack.len() - 2));
                        }

                        let step = DebugStep {
                            location: inst.location.clone(),
                            path: path.clone(),
                            variables_in_scope: vars_in_scope.clone(),
                            call_trace: local_call_trace.clone(),
                            kind: stmt_kind,
                            state_snapshot,
                        };

                        steps.push(step);

                        if matches!(inst.kind, InstructionKind::FunctionCall) {
                            expecting_function = true;

                            // add an entry into the call trace
                            call_trace.push(steps.len() - 1); // TODO: not sure it it should point to the call or the step, I think it is the call
                        }
                    }
                }
            }

            let is_revert = node
                .steps
                .last()
                .is_some_and(|step| step.op.get() == REVERT);

            // TODO Note: there are some weird mapping errors sometimes so we cannot rely on whether the statements revert or not
            // because this last opcode might not have a match into a statement.
            // Also it might happen that we do not have yet the statement parsed so it might be good that we catch anyway the revert.

            if node.kind.is_create() || is_revert {
                // get a copy before you pop
                let local_call_trace = call_trace.clone();

                // A constructor does not have an exit function call, so we have to manually add one
                // This function chunk includes an outgoing jump, which signals the exit of the function
                let Some(_) = call_trace.pop() else {
                    return Err(TraceError::FoundFunctionExitWithoutCall);
                };

                // Add the function exit step. We do this independent of whether we popped the function or not
                steps.push(DebugStep {
                    location: debug_unit.location,
                    path: debug_unit.path.clone(),
                    variables_in_scope: vec![],
                    call_trace: local_call_trace,
                    kind: StepKind::FunctionExit,
                    state_snapshot: Default::default(),
                })
            }
        }

        metrics_recorder.capture(Action::GenerateSteps);

        // loop over all the debug units and get the variable definitions
        let mut variable_definitions = HashMap::new();
        self.for_each_debug_unit(|_, debug_unit| {
            for variable in debug_unit.variables.iter() {
                variable_definitions.insert(variable.id, variable.clone());
            }

            // add the state variables from the debug unit, the debug unit only tracks
            // the ids so we have to use the context to retrieve the actual variables
            for state_variable_id in debug_unit.state_variables.iter() {
                let id = *state_variable_id;
                let var = trace_context
                    .state_variables
                    .get(&id)
                    .cloned()
                    .expect("state variable not found");

                variable_definitions.insert(id as u64, var);
            }
        });

        // resolve the type of all the variables we have in variable_definitions so far since
        // those are the ones we are going to be used in the trace
        let mut variable_types = HashMap::new();
        for variable in variable_definitions.values() {
            let typ = parse_variable_declaration_type(&variable.type_name, &trace_context.structs)
                .unwrap();
            variable_types.insert(variable.id, typ);
        }

        // we are doing it after the rest to make sure all the variables are included in variable_definitions
        self.for_each_debug_unit(|_, debug_unit| {
            // compute the assignemnts and offsets for the state variables that are not constants
            let non_constante_state_variables: Vec<(Variable, Type)> = debug_unit
                .state_variables
                .iter()
                .flat_map(|id| {
                    let id = *id as u64;
                    let var: Variable = variable_definitions
                        .get(&id)
                        .cloned()
                        .expect("variable not found");

                    if var.is_constant {
                        return None;
                    }
                    let typ = variable_types
                        .get(&var.id)
                        .cloned()
                        .expect("type not found");

                    Some((var, typ))
                })
                .collect();

            // put them in a tuple of the format (String, type) for the StateReference::compute_offsets
            let state_variables_as_tuple: Vec<(String, Type)> = non_constante_state_variables
                .iter()
                .map(|(var, typ)| (var.name.clone(), typ.clone()))
                .collect();

            let (offsets, _) = StateReference::compute_offsets(state_variables_as_tuple);

            for ((var, _), (_, offset)) in non_constante_state_variables.iter().zip(offsets.iter())
            {
                assignments.insert(var.id, Assignment::Storage(*offset));
            }
        });

        metrics_recorder.capture(Action::GenerateVariableDefinitions);

        Ok((
            DebugTrace {
                steps,
                variables: variable_definitions,
                variable_types,
                assignments,
                metrics: metrics_recorder.metrics,
            },
            trace_context,
        ))
    }
}

#[derive(Debug, Clone)]
pub struct DebugUnit {
    pub contract_name: String,
    pub path: String,
    pub location: SourceLocation,
    pub functions: HashMap<String, Function>,
    // list of state variables in this contract, they are stored in a
    // different place in the trace context
    pub state_variables: Vec<usize>,
    // list of all the variables in this contract
    pub variables: Vec<Variable>,
    pub source_id: u32,
    pub linearized_base_contracts: Vec<usize>,
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
pub struct SourceLocationHelper {
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
    #[allow(dead_code)]
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum VariableLocation {
    Storage,
    Memory,
    Stack,
    Calldata,
}

impl VariableLocation {
    fn from_str(s: &str, default: VariableLocation) -> Result<Self, StatementVisitorError> {
        match s {
            "storage" => Ok(VariableLocation::Storage),
            "memory" => Ok(VariableLocation::Memory),
            "stack" => Ok(VariableLocation::Stack),
            "calldata" => Ok(VariableLocation::Calldata),
            "default" => Ok(default),
            _ => Err(StatementVisitorError::UnexpectedStorageLocation(
                s.to_string(),
            )),
        }
    }
}

// Variable is a parsed representation of the Variable declaration NodeType in the AST.
#[derive(Debug, Clone, Serialize)]
pub struct Variable {
    pub name: String,
    pub id: u64,
    pub state_variable: bool, // TODO: Replace with location
    pub location: VariableLocation,
    pub is_constant: bool,
    pub type_name: Node,
}

#[derive(Debug, Clone, Serialize)]
pub enum Assignment {
    Storage(StoragePosition),
    Stack(usize),
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

fn is_forge_variable(name: &str) -> bool {
    SKIP_TRACE_LIST.contains(&name)
}

const SKIP_TRACE_LIST: &[&str] = &[
    "VM_ADDRESS",
    "CONSOLE",
    "CREATE2_FACTORY",
    "DEFAULT_SENDER",
    "DEFAULT_TEST_CONTRACT",
    "MULTICALL3_ADDRESS",
    "SECP256K1_ORDER",
    "UINT256_MAX",
    "vm",
    "stdstore",
    "vm",
    "_failed",
    "vm",
    "stdChainsInitialized",
    "chains",
    "defaultRpcUrls",
    "idToAlias",
    "fallbackToDefaultRpcUrls",
    "vm",
    "UINT256*MAX",
    "gasMeteringOff",
    "stdstore",
    "vm",
    "CONSOLE2_ADDRESS",
    "_excludedContracts",
    "_excludedSenders",
    "_targetedContracts",
    "_targetedSenders",
    "_excludedArtifacts",
    "_targetedArtifacts",
    "_targetedArtifactSelectors",
    "_excludedSelectors",
    "_targetedSelectors",
    "_targetedInterfaces",
    "multicall",
    "vm",
    "CONSOLE2_ADDRESS",
    "INT256_MIN_ABS",
    "SECP256K1_ORDER",
    "UINT256_MAX",
    "CREATE2_FACTORY",
    "IS_TEST",
    "FAILED_SLOT",
];

#[cfg(test)]
pub mod testing {
    use crate::debugger::tracer::DebugTrace;
    use crate::TraceArgs;

    pub fn trace_function(name: &str, functions: &str) -> eyre::Result<DebugTrace> {
        let contract = format!(
            "// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.0;

contract TestContract {{
    {}
}}
",
            functions,
        );

        generate_test_trace(name, "test", &contract)
    }

    fn generate_test_trace(
        test_file_name: &str,
        test_name: &str,
        contract: &str,
    ) -> eyre::Result<DebugTrace> {
        // write the file into testcases test/debug directory
        let workspace_path = std::env::var("CARGO_MANIFEST_DIR")? + "/src/debugger/testcases";
        let test_dir = workspace_path.clone() + "/test/debug";

        // TODO: https://github.com/ferranbt/solstice/issues/50
        let out_dir = std::path::Path::new(&workspace_path).join("out");
        if out_dir.exists() {
            std::fs::remove_dir_all(&out_dir)?;
        }

        let file_path = std::path::Path::new(&test_dir).join(format!("{}.t.sol", test_file_name));
        // Create all parent directories if they don't exist
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file_path, contract)?;

        let args = TraceArgs {
            workspace: Some(workspace_path),
            match_test: test_name.to_string(),
            match_path: file_path.to_str().unwrap().to_string(),
            dump: None,
            pprof: None,
            flamegraph: None,
        };

        args.execute_trace()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debugger::debugger::Debugger;
    use crate::debugger::state::testing::compile_contract;
    use crate::forge::Forge;
    use std::fmt::{Display, Write};
    use std::path::PathBuf;

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
        pub fn to_debug_format(
            &self,
            workspace_path: &str,
            debug_context: &TraceContext,
        ) -> String {
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

                // Get names of variables in scope
                let vars_in_scope: Vec<String> = step
                    .variables_in_scope
                    .iter()
                    .filter_map(|&id| {
                        // Try to find the variable first in the local debug context and then in
                        // the general one that stores the state variables
                        let var = if let Some(var) = self.variables.get(&(id as u64)) {
                            var
                        } else {
                            debug_context.state_variables.get(&id).unwrap()
                        };

                        let name = var.name.clone();
                        if SKIP_TRACE_LIST.contains(&name.as_str()) {
                            None
                        } else {
                            Some(name)
                        }
                    })
                    .collect();

                match &step.kind {
                    StepKind::FunctionDefinition(name) => {
                        writeln!(
                            output,
                            "{}[FUNC] {} ({}:{})",
                            indent, name, relative_path, step.location.line
                        )
                        .unwrap();
                    }
                    StepKind::FunctionExit => {
                        writeln!(
                            output,
                            "{}[EXIT] {}:{} scope=[{}]",
                            indent,
                            step.location.line,
                            step.location.column,
                            vars_in_scope.join(", ")
                        )
                        .unwrap();
                    }
                    StepKind::FunctionCall(kind) => {
                        writeln!(
                            output,
                            "{}[{}] {}:{}",
                            indent, kind, step.location.line, step.location.column
                        )
                        .unwrap();
                    }
                    StepKind::Statement(is_revert) => {
                        writeln!(
                            output,
                            "{}[STMT] {}:{} scope=[{}]{}",
                            indent,
                            step.location.line,
                            step.location.column,
                            vars_in_scope.join(", "),
                            if *is_revert { " (revert)" } else { "" }
                        )
                        .unwrap();
                    }
                    StepKind::Unknown => unimplemented!("Unknown step kind encountered"),
                }
            }
            output
        }
    }

    #[test]
    fn test_debugger_syntax() {
        let workspace_path_string = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR not set")
            + "/src/debugger/testcases";

        let workspace_path = workspace_path_string.as_str();
        let test_dir = Path::new(workspace_path).join("syntax");
        let mut trace_context = TraceContext::new();

        for entry in fs::read_dir(test_dir).unwrap() {
            let path = entry.unwrap().path();

            if path.is_file() && path.extension().unwrap() == "sol" {
                let expected_path = path.with_extension("syntax");

                let contract = fs::read_to_string(&path).unwrap();
                let artifact = compile_contract(&contract).unwrap();

                let deployed_bytecode = artifact.compact_bytecode_deployed();
                let bytecode = artifact.compact_bytecode();

                // populate the trace context with the contract because the Try catch call
                // references a contract. TODO: we should find a way to do this automatically
                // in a function to be consumed by this test.
                artifact.ast.nodes.iter().for_each(|node| {
                    trace_context.structs.insert(node.id.unwrap(), node.clone());
                });

                let contract_ast = artifact.ast.nodes.last().unwrap();

                let mut visitor = StatementVisitor::new(
                    "".to_string(),
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

    #[derive(Debug)]
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
            let test_path_no_ext_str = test_path.with_extension("").to_string_lossy().to_string();

            let trace_files = fs::read_dir(test_path.parent().unwrap())
                .unwrap()
                .filter_map(|entry| {
                    // Only keep the files which have the same prefix as 'test_path_no_ext'
                    let path = entry.unwrap().path();

                    let path_str = path.to_string_lossy().to_string();
                    if !path_str.starts_with(test_path_no_ext_str.clone().as_str()) {
                        return None;
                    }

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
            + "/src/debugger/testcases";
        let workspace_path = workspace_path_string.as_str();

        let filter_trace = std::env::var("FILTER_TRACE").unwrap_or_default();
        let override_tests = std::env::var("OVERRIDE_TESTS").unwrap_or_default() != "";

        let test_traces = get_trace_test_cases(workspace_path)
            .into_iter()
            .filter(|t| {
                // If the FILTER_TRACE env variable is set, only run the trace for the function
                // specified there.
                filter_trace.is_empty() || filter_trace == t.test_case_function
            })
            .collect::<Vec<_>>();

        if test_traces.is_empty() {
            return Err(eyre::eyre!("No test traces found"));
        }

        for trace_test_case in test_traces {
            // If the FILTER_TRACE env variable is set, only run the trace for the function
            // specified there.
            if !filter_trace.is_empty() && filter_trace != trace_test_case.test_case_function {
                continue;
            }

            println!(
                "Running trace for {} in {}",
                trace_test_case.test_case_function, trace_test_case.test_path
            );

            // TODO: There is an error when calling forge multiple times, the ast files are
            // not updated correctly. So, we need to remove the out directory on every test run.
            let out_dir = Path::new(workspace_path).join("out");
            if out_dir.exists() {
                fs::remove_dir_all(&out_dir)?;
            }

            // write the metadata information
            let log_output_dir = format!("trace/{}", timestamp);
            std::fs::create_dir_all(&log_output_dir)?;

            let debug_trace_path = {
                let path = std::path::absolute(format!("{}/forge_trace.json", log_output_dir))
                    .expect("Failed to get absolute path");
                path.to_string_lossy().into_owned()
            };

            let _ = Forge::new()
                .expect("Failed to find forge")
                .workspace_path(workspace_path)
                .debug(
                    &trace_test_case.test_case_function,
                    &trace_test_case.test_path,
                    debug_trace_path.as_str(),
                )
                .execute()?;

            let (debug_trace, trace_context) =
                Builder::new(workspace_path, debug_trace_path.as_str())
                    .unwrap()
                    .generate_trace()
                    .unwrap();

            // save the debug trace
            let debug_trace_json = serde_json::to_string(&debug_trace).unwrap();
            std::fs::write(
                format!("{}/debug_trace.json", log_output_dir),
                debug_trace_json,
            )?;

            let mut formatted = debug_trace.to_debug_format(workspace_path, &trace_context);

            let expected = fs::read_to_string(trace_test_case.expected_path.clone()).unwrap();
            if expected.contains("---") {
                // Include the state snapshot in the formatted output
                let mut debugger = Debugger::new(debug_trace);
                debugger.last();
                debugger.prev(); // Go back to the last statement since it will have both storage and stack variables

                let vars_in_scope = debugger.scope();

                // resolve each variable
                let mut state_result = String::new();
                for var in vars_in_scope {
                    if let Some(result) = debugger.get_variable(var.id).unwrap() {
                        // TODO: Handle function parameters
                        let value = &result.variables[0].value;
                        state_result += &format!("Variable: {} = {}\n", var.name, value);
                    }
                }

                formatted += &format!("---\n{}", state_result);
            }

            std::fs::write(
                format!("{}/debug_trace.txt", log_output_dir),
                formatted.clone(),
            )?;

            if override_tests {
                // Override the expected file with the new trace
                fs::write(trace_test_case.expected_path, formatted.clone())?;
                continue;
            }

            println!("{}", formatted);
            println!("{}", expected);

            assert_eq!(formatted, expected, "incorrect trace");
        }

        Ok(())
    }
}
