use alloy_primitives::hex;
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_primitives::FixedBytes;
use alloy_primitives::Signed;
use alloy_primitives::I256;
use alloy_primitives::I8;
use alloy_primitives::U256;
use arbitrary::{Arbitrary, Unstructured};
use core::panic;
use foundry_compilers::artifacts::ast::{self, Ast, Node, NodeType};
use foundry_compilers::artifacts::sourcemap::parse;
use foundry_compilers::artifacts::BytecodeObject;
use foundry_compilers::artifacts::CompactBytecode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::env::var;
use std::io::Read;
use std::io::Write;
use std::num;
use std::process::Command;
use std::process::Stdio;
use std::str::FromStr;

use crate::tracer::IcPcMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    /// `address $(payable)?`
    Address,
    /// `bool`
    Bool,
    /// `string`
    String,
    /// `bytes`
    Bytes,
    /// `bytes<size>`
    FixedBytes(u16),

    /// `int[size]`
    Int(Option<u16>),
    /// `uint[size]`
    Uint(Option<u16>),

    /// `$ty[$($size)?]`
    Array(TypeArray),
    /// `$(tuple)? ( $($types,)* )`
    Tuple(TypeTuple),
    /// `mapping($key $($key_name)? => $value $($value_name)?)`
    Mapping(TypeMapping),
}

impl Type {
    pub fn decode_bytes(&self, bytes: &[u8]) -> Result<JsonValue, String> {
        match self {
            Type::Address => {
                let addr = Address::from_slice(bytes);
                Ok(serde_json::Value::String(addr.to_string()))
            }
            Type::Bool => {
                // len should be 1 byte
                let byte = bytes[0];
                Ok(serde_json::Value::Bool(byte != 0))
            }
            Type::Uint(_) => {
                let value = U256::from_be_slice(&bytes);

                Ok(serde_json::Value::String(value.to_string()))
            }
            Type::Int(size) => match size {
                Some(8) => {
                    let value = i8::from_be_bytes(bytes.try_into().unwrap());
                    Ok(serde_json::Value::String(value.to_string()))
                }
                Some(16) => {
                    let value = i16::from_be_bytes(bytes.try_into().unwrap());
                    Ok(serde_json::Value::String(value.to_string()))
                }
                Some(32) => {
                    let value = i32::from_be_bytes(bytes.try_into().unwrap());
                    Ok(serde_json::Value::String(value.to_string()))
                }
                Some(64) => {
                    let value = i64::from_be_bytes(bytes.try_into().unwrap());
                    Ok(serde_json::Value::String(value.to_string()))
                }
                Some(128) => {
                    let value = i128::from_be_bytes(bytes.try_into().unwrap());
                    Ok(serde_json::Value::String(value.to_string()))
                }
                Some(256) => {
                    let value = I256::from_be_bytes::<32>(bytes.try_into().unwrap());
                    Ok(serde_json::Value::String(value.to_string()))
                }
                _ => Err("Unsupported int size".to_string()),
            },
            Type::String => {
                // Convert bytes to UTF-8 string
                match String::from_utf8(bytes.to_vec()) {
                    Ok(s) => Ok(serde_json::Value::String(s)),
                    Err(_) => Err("Failed to convert bytes to string".to_string()),
                }
            }
            Type::FixedBytes(_) | Type::Bytes => Ok(serde_json::Value::String(format!(
                "0x{}",
                hex::encode(bytes)
            ))),
            _ => Err("Unsupported type".to_string()),
        }
    }

    pub fn get_bytes(&self) -> u32 {
        match self {
            Type::Address => 20,
            Type::Bool => 1,
            Type::String => 32,
            Type::Bytes => 32,
            Type::FixedBytes(size) => *size as u32,
            Type::Int(size) => match size {
                Some(s) => (*s as u32) / 8,
                None => 32, // default int256
            },
            Type::Uint(size) => match size {
                Some(s) => (*s as u32) / 8,
                None => 32, // default uint256
            },
            Type::Array(_) => 32,
            Type::Tuple(_) => 32,
            Type::Mapping(_) => 32,
        }
    }

    pub fn num_storage_slots(&self) -> u32 {
        match self {
            Type::Bytes
            | Type::String
            | Type::Bool
            | Type::Address
            | Type::FixedBytes(_)
            | Type::Int(_)
            | Type::Uint(_) => 1,
            Type::Array(TypeArray { ty, size }) => match size {
                None => 1,
                Some(size) => {
                    let size = *size as u32;

                    let underlying_bytes = ty.get_bytes();
                    if underlying_bytes < 32 {
                        let items_per_slot = 32 / underlying_bytes;
                        (size + items_per_slot - 1) / items_per_slot
                    } else {
                        size * ty.num_storage_slots()
                    }
                }
            },
            Type::Tuple(TypeTuple { types }) => {
                let (_offsets, last) = StateReference::compute_offsets(types.to_vec());
                U256::from_be_bytes(last.slot.0).as_limbs()[0] as u32
            }
            Type::Mapping(TypeMapping { key, value }) => !unimplemented!(),
        }
    }

    pub fn is_dynamic(&self) -> bool {
        match self {
            Type::Array(arr) => arr.size.is_none(),
            Type::Tuple(_tuple) => true,
            Type::String => true,
            Type::Bytes => true,
            _ => false,
        }
    }
}

// skipping mapping for now since it is a bit more complicated to test and it requires the preimages
impl<'a> Arbitrary<'a> for Type {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        match u.choose(&[
            0, // Address
            1, // Bool
            2, // String
            3, // Bytes
            4, // FixedBytes
            5, // Int
            6, // Uint
            7, // Array
            8, // Tuple
               //9, // Mapping
        ])? {
            0 => Ok(Type::Address),
            1 => Ok(Type::Bool),
            2 => Ok(Type::String),
            3 => Ok(Type::Bytes),
            4 => {
                // Generate a size between 1 and 32 for FixedBytes
                let size = u.int_in_range(1..=32)?;
                Ok(Type::FixedBytes(size))
            }
            5 => {
                // Int sizes: 8, 16, 32, 64, 128, 256
                let sizes = [8u16, 16, 32, 64, 128, 256];
                let size = *u.choose(&sizes)?;
                Ok(Type::Int(Some(size)))
            }
            6 => {
                // Uint sizes: 8, 16, 32, 64, 128, 256
                let sizes = [8u16, 16, 32, 64, 128, 256];
                let size = *u.choose(&sizes)?;
                Ok(Type::Uint(Some(size)))
            }
            7 => Ok(Type::Array(TypeArray::arbitrary(u)?)),
            8 => Ok(Type::Tuple(TypeTuple::arbitrary(u)?)),
            //9 => Ok(Type::Mapping(TypeMapping::arbitrary(u)?)),
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, Debug)]
struct SolidityValue {
    setup_code: String,
    value: JsonValue,
}

struct ContractGenerator<'a> {
    typ: Type,
    structs: Vec<String>,
    u: &'a mut Unstructured<'a>,
}

impl<'a> ContractGenerator<'a> {
    pub fn new(typ: Type, u: &'a mut Unstructured<'a>) -> Self {
        Self {
            typ,
            structs: Vec::new(),
            u,
        }
    }

    pub fn build_memory(typ: Type, u: &'a mut Unstructured<'a>) -> GeneratedContract {
        let mut generator = ContractGenerator::new(typ, u);
        generator.build_memory_inner()
    }

    pub fn build_storage(typ: Type, u: &'a mut Unstructured<'a>) -> GeneratedContract {
        let mut generator = ContractGenerator::new(typ, u);
        generator.build_storage_inner()
    }

    pub fn build_stack(typ: Type, u: &'a mut Unstructured<'a>) -> GeneratedContract {
        let mut generator = ContractGenerator::new(typ, u);
        generator.build_stack_inner()
    }

    pub fn build_storage_inner(&mut self) -> GeneratedContract {
        let tuple = match self.typ.clone() {
            Type::Tuple(tuple) => tuple,
            _ => panic!("Type is not a tuple"),
        };

        // First collect all struct definitions by running encode_argument
        let mut state_vars = Vec::new();
        for (inx, (_, ty)) in tuple.types.iter().enumerate() {
            let name = format!("arg_{}", inx);
            let type_str = self.encode_argument(&name, ty.clone());
            state_vars.push((name, type_str));
        }

        // Then format the full contract
        let struct_defs = self.structs.join("\n\n    ");
        let vars = state_vars
            .iter()
            .map(|(k, v)| format!("    {} {};", v, k))
            .collect::<Vec<_>>()
            .join("\n");

        // Generate setup code and collect values
        let mut setup_code = String::new();
        let mut values = serde_json::Map::new();

        for (inx, (_, ty)) in tuple.types.iter().enumerate() {
            let name = format!("arg_{}", inx);
            let (setup, value) = self.generate_setup_code_2_with_value(&name, ty.clone());
            setup_code.push_str(&setup);
            setup_code.push('\n');
            values.insert(name, value);
        }

        let setup_code = setup_code.trim_end();

        let contract = format!(
            "// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.0;

contract ComplexTypes {{
    {}
    {}

    function setup() public {{
        {}
    }}
}}
",
            struct_defs, vars, setup_code,
        );

        GeneratedContract {
            source: contract,
            values: JsonValue::Object(values),
        }
    }

    pub fn encode_argument(&mut self, field_name: &str, ty: Type) -> String {
        match ty {
            Type::Tuple(tuple) => {
                // Check if we've already generated this exact struct
                let mut fields = String::new();
                for (inx, (_, ty)) in tuple.types.iter().enumerate() {
                    fields.push_str(&format!(
                        "{} {};\n",
                        self.encode_argument(&format!("field_{}", inx), ty.clone()),
                        format!("field_{}", inx)
                    ));
                }

                // Generate a new struct name
                let struct_name = format!("Struct{}", self.structs.len() + 1);

                // Add the struct definition
                self.structs
                    .push(format!("struct {} {{\n{}    }}", struct_name, fields));
                struct_name
            }
            Type::Array(arr) => {
                let base = self.encode_argument(field_name, *arr.ty);
                match arr.size {
                    Some(size) => format!("{}[{}]", base, size),
                    None => format!("{}[]", base),
                }
            }
            Type::Mapping(mapping) => {
                let key = self.encode_argument(field_name, *mapping.key);
                let value = self.encode_argument(field_name, *mapping.value);
                format!("mapping({} => {})", key, value)
            }
            Type::Address => "address".to_string(),
            Type::Bool => "bool".to_string(),
            Type::String => "string".to_string(),
            Type::Bytes => "bytes".to_string(),
            Type::FixedBytes(size) => format!("bytes{}", size),
            Type::Int(size) => match size {
                Some(s) => format!("int{}", s),
                None => "int256".to_string(),
            },
            Type::Uint(size) => match size {
                Some(s) => format!("uint{}", s),
                None => "uint256".to_string(),
            },
        }
    }

    fn generate_value(&mut self, field_name: &str, ty: &Type) -> SolidityValue {
        match ty {
            Type::Address
            | Type::Bool
            | Type::String
            | Type::Bytes
            | Type::FixedBytes(_)
            | Type::Int(_)
            | Type::Uint(_) => {
                // Simple types just need a single value assignment
                let value = self.generate_random_value(ty);
                SolidityValue {
                    setup_code: format!("{} = {};", field_name, self.value_to_literal(&value, ty)),
                    value,
                }
            }
            Type::Array(arr) => self.generate_array_value(field_name, arr),
            Type::Tuple(tuple) => self.generate_struct_value(field_name, tuple),
            Type::Mapping(mapping) => self.generate_mapping_value(field_name, mapping),
        }
    }

    fn generate_array_value(&mut self, field_name: &str, arr: &TypeArray) -> SolidityValue {
        let mut setup = String::new();
        let mut values = Vec::new();

        match arr.size {
            Some(size) => {
                // Fixed size array
                for i in 0..size {
                    let element = self.generate_value(&format!("{}[{}]", field_name, i), &arr.ty);
                    setup.push_str(&element.setup_code);
                    setup.push('\n');
                    values.push(element.value);
                }
            }
            None => {
                // Dynamic array
                for _ in 0..2 {
                    setup.push_str(&format!("{}.push();\n", field_name));
                    let element = self.generate_value(
                        &format!("{}[{}.length - 1]", field_name, field_name),
                        &arr.ty,
                    );
                    setup.push_str(&element.setup_code);
                    setup.push('\n');
                    values.push(element.value);
                }
            }
        }

        SolidityValue {
            setup_code: setup.trim_end().to_string(),
            value: JsonValue::Array(values),
        }
    }

    fn generate_struct_value(&mut self, field_name: &str, tuple: &TypeTuple) -> SolidityValue {
        let mut setup = String::new();
        let mut values = serde_json::Map::new();

        for (idx, (_, field_ty)) in tuple.types.iter().enumerate() {
            let field = self.generate_value(&format!("{}.field_{}", field_name, idx), field_ty);
            setup.push_str(&field.setup_code);
            setup.push('\n');
            values.insert(format!("field_{}", idx), field.value);
        }

        SolidityValue {
            setup_code: setup.trim_end().to_string(),
            value: JsonValue::Object(values),
        }
    }

    fn generate_mapping_value(&mut self, field_name: &str, mapping: &TypeMapping) -> SolidityValue {
        let mut setup = String::new();
        let mut values = serde_json::Map::new();

        for _ in 0..2 {
            let key = self.generate_random_value(&mapping.key);
            let key_literal = self.value_to_literal(&key, &mapping.key);

            let value =
                self.generate_value(&format!("{}[{}]", field_name, key_literal), &mapping.value);
            setup.push_str(&value.setup_code);
            setup.push('\n');
            values.insert(key.to_string(), value.value);
        }

        SolidityValue {
            setup_code: setup.trim_end().to_string(),
            value: JsonValue::Object(values),
        }
    }

    pub fn generate_setup_code_2_with_value(
        &mut self,
        field_name: &str,
        ty: Type,
    ) -> (String, JsonValue) {
        let result = self.generate_value(field_name, &ty);
        (result.setup_code, result.value)
    }

    fn generate_random_value(&mut self, ty: &Type) -> JsonValue {
        match ty {
            Type::Bool => {
                let value = self.u.arbitrary::<bool>().unwrap_or(false);
                JsonValue::Bool(value)
            }
            Type::String => {
                // Generate a random string between 0 and 60 chars
                let length = self.u.int_in_range(0..=60).unwrap_or(10);
                let chars: String = (0..length)
                    .map(|_| {
                        // Use basic ASCII for readability
                        let idx = self.u.int_in_range(b'a'..=b'z').unwrap_or(b'x');
                        idx as char
                    })
                    .collect();
                JsonValue::String(chars)
            }
            Type::Bytes | Type::FixedBytes(_) | Type::Address => {
                let length = match ty {
                    Type::Bytes => self.u.int_in_range(0..=60).unwrap_or(10),
                    Type::FixedBytes(size) => *size as usize,
                    Type::Address => 20,
                    _ => panic!("Invalid type"),
                };

                let bytes: String = (0..length)
                    .map(|_| {
                        let byte = self.u.int_in_range(0..=255).unwrap_or(0);
                        format!("{:02x}", byte)
                    })
                    .collect();

                if *ty == Type::Address {
                    let addr = alloy_primitives::Address::from_str(bytes.as_str()).unwrap();
                    JsonValue::String(addr.to_string())
                } else {
                    JsonValue::String(format!("0x{}", bytes))
                }
            }
            Type::Int(size) => {
                let bytes = match size {
                    Some(s) => (s / 8) as u32,
                    None => 32,
                };
                // Just generate a small number that fits in the bytes
                let num = if bytes <= 4 {
                    self.u.int_in_range(-10i32..=10i32).unwrap_or(0)
                } else {
                    self.u.int_in_range(-1000i32..=1000i32).unwrap_or(0)
                };
                JsonValue::String(num.to_string())
            }
            Type::Uint(size) => {
                let bytes = match size {
                    Some(s) => (s / 8) as u32,
                    None => 32,
                };
                // Just generate a small positive number that fits in the bytes
                let num = if bytes <= 4 {
                    self.u.int_in_range(0u32..=10u32).unwrap_or(0)
                } else {
                    self.u.int_in_range(0u32..=1000u32).unwrap_or(0)
                };
                JsonValue::String(num.to_string())
            }
            _ => JsonValue::Null,
        }
    }

    fn value_to_literal(&self, value: &JsonValue, ty: &Type) -> String {
        match value {
            JsonValue::String(s) if s.starts_with("0x") => {
                match ty {
                    Type::Address => s.clone(), // Keep addresses as 0x...
                    Type::FixedBytes(_) => format!("hex\"{}\"", &s[2..]), // Convert fixed bytes to hex"..."
                    Type::Bytes => format!("hex\"{}\"", &s[2..]), // Convert dynamic bytes to hex"..."
                    _ => format!("hex\"{}\"", &s[2..]),
                }
            }
            JsonValue::String(s) => match ty {
                Type::Int(_) | Type::Uint(_) => s.clone(),
                _ => format!("\"{}\"", s),
            },
            JsonValue::Number(n) => n.to_string(),
            JsonValue::Bool(b) => b.to_string(),
            _ => "0".to_string(),
        }
    }

    pub fn build_memory_inner(&mut self) -> GeneratedContract {
        let tuple = match self.typ.clone() {
            Type::Tuple(tuple) => tuple,
            _ => panic!("Type is not a tuple"),
        };

        // First collect all struct definitions and build type mapping
        let mut state_vars = Vec::new();
        let mut type_mapping = HashMap::new();

        for (inx, (_, ty)) in tuple.types.iter().enumerate() {
            let name = format!("arg_{}", inx);
            let type_str = self.encode_argument(&name, ty.clone());
            type_mapping.insert(inx, type_str.clone());
            state_vars.push((name, type_str));
        }

        // Format the full contract
        let struct_defs = self.structs.join("\n\n    ");

        // Generate setup code and collect values
        let mut setup_code = String::new();
        let mut values = serde_json::Map::new();

        // First declare all variables
        for (inx, (name, type_str)) in state_vars.iter().enumerate() {
            let declaration = match &tuple.types[inx].1 {
                Type::Array(arr) => {
                    if arr.size.is_none() {
                        // For dynamic arrays, use the stored type mapping
                        let array_type = match type_mapping.get(&inx).unwrap().strip_suffix("[]") {
                            Some(base_type) => base_type,
                            None => type_mapping.get(&inx).unwrap(), // Fallback
                        };
                        format!(
                            "    {} memory {} = new {}[]({});\n",
                            type_str, name, array_type, 2
                        )
                    } else {
                        format!("    {} memory {};\n", type_str, name)
                    }
                }
                _ => format!("    {} memory {};\n", type_str, name),
            };
            setup_code.push_str(&declaration);
        }

        // Then generate the assignments
        for (inx, (_, ty)) in tuple.types.iter().enumerate() {
            let name = format!("arg_{}", inx);
            let (setup, value) = self.generate_setup_code_memory(&name, ty.clone());
            setup_code.push_str(&setup);
            setup_code.push('\n');
            values.insert(name, value);
        }

        let setup_code = setup_code.trim_end();

        let contract = format!(
            "// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.0;

contract ComplexTypes {{
    {}

    function setup() public {{
{}
    }}
}}
",
            struct_defs, setup_code,
        );

        GeneratedContract {
            source: contract,
            values: JsonValue::Object(values),
        }
    }

    fn generate_setup_code_memory(&mut self, field_name: &str, ty: Type) -> (String, JsonValue) {
        match ty {
            Type::Array(arr) => {
                let mut setup = String::new();
                let mut values = Vec::new();

                match arr.size {
                    Some(size) => {
                        // Fixed size array
                        for i in 0..size {
                            let (element_setup, element_value) = self.generate_setup_code_memory(
                                &format!("{}[{}]", field_name, i),
                                *arr.ty.clone(),
                            );
                            setup.push_str(&element_setup);
                            setup.push('\n');
                            values.push(element_value);
                        }
                    }
                    None => {
                        // Dynamic array - already initialized with new
                        for i in 0..2 {
                            let (element_setup, element_value) = self.generate_setup_code_memory(
                                &format!("{}[{}]", field_name, i),
                                *arr.ty.clone(),
                            );
                            setup.push_str(&element_setup);
                            setup.push('\n');
                            values.push(element_value);
                        }
                    }
                }

                (setup.trim_end().to_string(), JsonValue::Array(values))
            }
            Type::Tuple(tuple) => {
                let mut setup = String::new();
                let mut values = serde_json::Map::new();

                for (idx, (_, field_ty)) in tuple.types.iter().enumerate() {
                    let (field_setup, field_value) = self.generate_setup_code_memory(
                        &format!("{}.field_{}", field_name, idx),
                        field_ty.clone(),
                    );
                    setup.push_str(&field_setup);
                    setup.push('\n');
                    values.insert(format!("field_{}", idx), field_value);
                }

                (setup.trim_end().to_string(), JsonValue::Object(values))
            }
            _ => {
                let value = self.generate_random_value(&ty);
                (
                    format!("{} = {};", field_name, self.value_to_literal(&value, &ty)),
                    value,
                )
            }
        }
    }

    pub fn build_stack_inner(&mut self) -> GeneratedContract {
        let tuple = match self.typ.clone() {
            Type::Tuple(tuple) => tuple,
            _ => panic!("Type is not a tuple"),
        };

        // For stack variables, we need declarations and assignments
        let mut declarations = String::new();
        let mut assignments = String::new();
        let mut values = serde_json::Map::new();

        for (inx, (_, ty)) in tuple.types.iter().enumerate() {
            // Only allow simple types for stack
            match ty {
                Type::Address | Type::Bool | Type::Uint(_) | Type::Int(_) | Type::FixedBytes(_) => {
                    let name = format!("arg_{}", inx);
                    let type_str = self.encode_argument(&name, ty.clone());

                    // Declare the variable
                    declarations.push_str(&format!("        {} {};\n", type_str, name));

                    // Generate and assign the value
                    let value = self.generate_random_value(ty);
                    assignments.push_str(&format!(
                        "        {} = {};\n",
                        name,
                        self.value_to_literal(&value, ty)
                    ));
                    values.insert(name, value);
                }
                _ => continue, // Skip complex types
            }
        }

        let contract = format!(
            "// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.0;

contract ComplexTypes {{
    function setup() public {{
        // Stack variables declarations
{}
        // Stack variables assignments
{}
    }}
}}",
            declarations.trim_end(),
            assignments.trim_end()
        );

        GeneratedContract {
            source: contract,
            values: JsonValue::Object(values),
        }
    }
}

// Memory type is a type that is only allocated in memory, this is done to apply a subset and
// use arbitary on that subset
#[derive(Clone, Debug, PartialEq)]
enum MemoryType {
    /// `$ty[$($size)?]`
    Array(TypeArray),
    /// `$(tuple)? ( $($types,)* )`
    Tuple(TypeTuple),
}

impl MemoryType {
    fn arbitrary_type(u: &mut Unstructured) -> arbitrary::Result<Type> {
        // Get number of elements (1-5)
        let num_elements = u.int_in_range(1..=5)?;

        // Generate that many memory types and convert to Type
        let mut types = Vec::new();
        for _ in 0..num_elements {
            let memory_type = MemoryType::arbitrary(u)?;
            let typ = Type::from(memory_type);
            types.push(("".to_string(), typ));
        }

        Ok(Type::Tuple(TypeTuple { types }))
    }
}

impl<'a> Arbitrary<'a> for MemoryType {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        match u.choose(&[
            0, // Array
            1, // Tuple
        ])? {
            0 => Ok(MemoryType::Array(TypeArray::arbitrary(u)?)),
            1 => Ok(MemoryType::Tuple(TypeTuple::arbitrary(u)?)),
            _ => unreachable!(),
        }
    }
}

impl From<MemoryType> for Type {
    fn from(memory_type: MemoryType) -> Self {
        match memory_type {
            MemoryType::Array(array) => Type::Array(array),
            MemoryType::Tuple(tuple) => Type::Tuple(tuple),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeArray {
    pub ty: Box<Type>,
    pub size: Option<u16>,
}

impl<'a> Arbitrary<'a> for TypeArray {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let ty = Box::new(Type::arbitrary(u)?);

        // First decide if it should be fixed or dynamic size
        let has_size = bool::arbitrary(u)?;

        let size = if has_size {
            // Generate a size between 1 and 9
            Some(u.int_in_range(1..=9)?)
        } else {
            None
        };

        Ok(TypeArray { ty, size })
    }
}

/// A tuple type.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeTuple {
    pub types: Vec<(String, Type)>,
}

impl<'a> Arbitrary<'a> for TypeTuple {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // Generate a size between 1 and 5 (at least one field, max 5 fields)
        let size = u.int_in_range(1..=5)?;

        let mut types = Vec::with_capacity(size as usize);
        for i in 0..size {
            // Generate a type but avoid mappings inside structs
            let ty = loop {
                let potential_type = Type::arbitrary(u)?;
                match potential_type {
                    Type::Mapping(_) => continue, // Skip mappings in structs
                    _ => break potential_type,
                }
            };
            types.push((format!("field_{}", i), ty));
        }

        Ok(TypeTuple { types })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeMapping {
    pub key: Box<Type>,
    pub value: Box<Type>,
}

impl<'a> Arbitrary<'a> for TypeMapping {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // Generate a valid key type (only simple types allowed as keys)
        let key = match u.choose(&[
            0, // Address
            1, // String
            2, // Bytes
            3, // FixedBytes
            4, // Int
            5, // Uint
        ])? {
            0 => Type::Address,
            1 => Type::String,
            2 => Type::Bytes,
            3 => {
                let size = u.int_in_range(1..=9)?;
                Type::FixedBytes(size)
            }
            4 => {
                let sizes = [8u16, 16, 32, 64, 128, 256];
                let size = *u.choose(&sizes)?;
                Type::Int(Some(size))
            }
            5 => {
                let sizes = [8u16, 16, 32, 64, 128, 256];
                let size = *u.choose(&sizes)?;
                Type::Uint(Some(size))
            }
            _ => unreachable!(),
        };

        // Value can be any type
        let value = Type::arbitrary(u)?;

        Ok(TypeMapping {
            key: Box::new(key),
            value: Box::new(value),
        })
    }
}

enum VariableKind {
    Storage(u64),
    Memory(u64),
    Stack(u64),
}

struct StateReference {
    storage: HashMap<FixedBytes<32>, FixedBytes<32>>,
    memory: Bytes,
    stack: Vec<Bytes>,
}

#[derive(Default, Clone, Debug)]
struct StoragePosition {
    slot: FixedBytes<32>,
    index_in_slot: u32,
}

impl StoragePosition {
    fn set_slot(&mut self, indx: u32) {
        self.slot = FixedBytes::<32>::from_slice(&U256::from(indx).to_be_bytes::<32>());
        self.index_in_slot = 0;
    }

    fn advance_slot_num(&mut self, num: u32) {
        self.slot = FixedBytes::<32>::from_slice(
            &(U256::from_be_bytes(self.slot.0) + U256::from(num)).to_be_bytes::<32>(),
        );
        self.index_in_slot = 0;
    }

    fn advance_slot(&mut self) {
        self.slot = FixedBytes::<32>::from_slice(
            &(U256::from_be_bytes(self.slot.0) + U256::from(1)).to_be_bytes::<32>(),
        );
        self.index_in_slot = 0;
    }

    fn move_index(&mut self, num_bytes: u32) {
        self.index_in_slot += num_bytes;
    }

    fn slot_as_u32(&self) -> u32 {
        U256::from_be_bytes(self.slot.0).as_limbs()[0] as u32
    }
}

const SLOT_SIZE: u32 = 32;

impl StateReference {
    fn new(storage: HashMap<FixedBytes<32>, FixedBytes<32>>) -> Self {
        Self {
            storage,
            memory: Bytes::new(),
            stack: Vec::new(),
        }
    }

    fn resolve_type(&self, ty: Type, offset: StoragePosition) -> JsonValue {
        match ty {
            Type::Address | Type::Bool | Type::FixedBytes(_) | Type::Uint(_) | Type::Int(_) => {
                let num_bytes = ty.get_bytes();

                let slot_bytes = if let Some(slot_bytes) = self.storage.get(&offset.slot) {
                    slot_bytes
                } else {
                    // retunr an empty array of FixedBytes<32>
                    let empty = FixedBytes::<32>::default();
                    &empty.clone()
                };

                let start = (SLOT_SIZE - offset.index_in_slot - num_bytes) as usize;
                let bytes = &slot_bytes[start..(start + num_bytes as usize)];

                ty.decode_bytes(bytes).unwrap()
            }
            Type::String | Type::Bytes => {
                let slot_bytes = if let Some(slot_bytes) = self.storage.get(&offset.slot) {
                    slot_bytes
                } else {
                    // if the type is bytes, return empty as 0x, if it is string, return empty as ""
                    if ty == Type::Bytes {
                        return JsonValue::String("0x".to_string());
                    } else {
                        return JsonValue::String("".to_string());
                    }
                };

                // Convert the full slot to a number to check if it's odd (separate storage)
                let length = U256::from_be_bytes(slot_bytes.0);

                let data = if length.bit(0) {
                    let length = length.as_limbs()[0] as usize;

                    // If length is odd, data is stored separately starting at keccak256(slot)
                    // Actual length is (length - 1) / 2
                    let actual_length = (length - 1) / 2;
                    let mut data = Vec::with_capacity(actual_length);

                    // Calculate starting slot for data
                    let mut data_slot = alloy_primitives::keccak256(offset.slot);

                    // Keep reading slots until we have all the data
                    while data.len() < actual_length {
                        let remaining = actual_length - data.len();

                        if let Some(word) = self.storage.get(&data_slot) {
                            // For all slots except possibly the last one, take all 32 bytes
                            let to_take = std::cmp::min(32, remaining);

                            data.extend_from_slice(&word.0[..to_take]);
                        } else {
                            // the other slots do not exist, so fill the remaning with 0
                            let zeros = vec![0u8; remaining];
                            data.extend_from_slice(&zeros);
                            break;
                        }

                        // Move to next slot
                        data_slot = FixedBytes::<32>::from_slice(
                            &(U256::from_be_bytes(data_slot.0) + U256::from(1)).to_be_bytes::<32>(),
                        );
                    }

                    data
                } else {
                    // If length is even, data is stored inline
                    // Length is the last byte / 2
                    let size = (slot_bytes.0[31] / 2) as usize;
                    let data = &slot_bytes.0[..size];

                    data.to_vec()
                };

                ty.decode_bytes(&data).unwrap()
            }
            Type::Array(TypeArray { ty, size }) => {
                let mut values = Vec::new();
                if let Some(size) = size {
                    let mut element_position = offset.clone();

                    for _i in 0..size {
                        // For fixed-size arrays, elements are stored sequentially
                        let val = self.resolve_type(*ty.clone(), element_position.clone());

                        // Advance position based on the element type
                        let (typ_num_slots, typ_num_bytes) =
                            (ty.num_storage_slots(), ty.get_bytes());

                        // For dynamic types (like bytes) or types that take multiple slots,
                        // we need to advance by the full slot
                        if ty.is_dynamic() || typ_num_slots > 1 {
                            element_position.advance_slot_num(typ_num_slots);
                        } else {
                            // For value types that fit in a single slot, we can pack them
                            element_position.move_index(typ_num_bytes);
                            if element_position.index_in_slot + typ_num_bytes > SLOT_SIZE {
                                element_position.advance_slot();
                            }
                        }

                        values.push(val);
                    }
                } else {
                    // Get length from current slot
                    let length_bytes = if let Some(bytes) = self.storage.get(&offset.slot) {
                        bytes
                    } else {
                        panic!("Could not find length at slot {}", hex::encode(offset.slot));
                    };
                    let length = U256::from_be_bytes(length_bytes.0).as_limbs()[0] as u32;

                    // Calculate starting slot for data
                    let data_slot = alloy_primitives::keccak256(offset.slot);
                    let mut element_position = StoragePosition {
                        slot: data_slot,
                        index_in_slot: 0,
                    };

                    let (typ_num_slots, typ_num_bytes) = (ty.num_storage_slots(), ty.get_bytes());

                    for i in 0..length {
                        let val = self.resolve_type(*ty.clone(), element_position.clone());

                        if typ_num_slots == 1
                            && element_position.index_in_slot + typ_num_bytes <= SLOT_SIZE
                        {
                            element_position.move_index(typ_num_bytes);
                            if element_position.index_in_slot + typ_num_bytes > SLOT_SIZE {
                                element_position.advance_slot();
                            }
                        } else {
                            element_position.advance_slot_num(typ_num_slots);
                        }

                        values.push(val);
                    }
                }
                JsonValue::Array(values)
            }
            Type::Tuple(TypeTuple { types }) => {
                let mut map = serde_json::Map::new();
                let (offsets, last_storage) = StateReference::compute_offsets(types.clone());

                for ((name, ty), (_, local_offset)) in types.into_iter().zip(offsets.into_iter()) {
                    let mut global_location = offset.clone();

                    global_location.advance_slot_num(local_offset.slot_as_u32());
                    global_location.move_index(local_offset.index_in_slot);

                    map.insert(name, self.resolve_type(ty.clone(), global_location));
                }
                JsonValue::Object(map)
            }
            Type::Mapping(_) => {
                JsonValue::Null // or handle mapping values if needed
            }
        }
    }

    pub fn compute_offsets(
        vars: Vec<(String, Type)>,
    ) -> (Vec<(String, StoragePosition)>, StoragePosition) {
        let mut position: StoragePosition = Default::default();
        let mut offsets: Vec<(String, StoragePosition)> = Vec::new();

        for (name, ty) in vars {
            // Check if we need to advance to next slot based on type size
            let num_bytes = ty.get_bytes();

            let bytes_left = SLOT_SIZE - position.index_in_slot;
            if num_bytes > bytes_left {
                position.advance_slot();
            }

            offsets.push((name, position.clone()));

            let num_slots = ty.num_storage_slots() as u32;
            if num_slots == 1 && position.index_in_slot + num_bytes <= SLOT_SIZE {
                position.move_index(num_bytes);
            } else {
                position.advance_slot_num(num_slots);
            }
        }

        if position.index_in_slot > 0 {
            position.advance_slot();
        }

        (offsets, position)
    }

    pub fn resolve_vars(&self, vars: Vec<(String, Type)>) -> JsonValue {
        let mut map = serde_json::Map::new();

        let (offsets, last_storage) = StateReference::compute_offsets(vars.clone());

        for ((name, ty), (_, offset)) in vars.into_iter().zip(offsets.into_iter()) {
            map.insert(name, self.resolve_type(ty, offset));
        }

        JsonValue::Object(map)
    }
}

#[derive(Deserialize, Debug)]
struct TypeNode {
    #[serde(rename = "nodeType")]
    pub node_type: TypeNodeType,

    /// Node attributes that were not deserialized.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

impl TypeNode {
    /// Deserialize a serialized node attribute.
    pub fn attribute<D: DeserializeOwned>(&self, key: impl AsRef<str>) -> Option<D> {
        // TODO: Can we avoid this clone?
        self.other
            .get(key.as_ref())
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

#[derive(Deserialize, Debug)]
enum TypeNodeType {
    UserDefinedTypeName,
    ElementaryTypeName,
    Mapping,
    ArrayTypeName,
    Literal,
}

fn parse_variable_declaration_type(typ: &TypeNode, structs: &HashMap<usize, Node>) -> Type {
    match typ.node_type {
        TypeNodeType::UserDefinedTypeName => {
            let reference_declaration = typ.attribute::<usize>("referencedDeclaration").unwrap();
            let struct_declaration = structs.get(&reference_declaration).unwrap();

            let members = struct_declaration
                .attribute::<Vec<Node>>("members")
                .unwrap();

            // loop over members, extrac typeName as TypeName and call parse_variable_declaration_type
            let mut inner_types = Vec::new();
            for member in members {
                let name = member.attribute::<String>("name").unwrap();

                let type_name = member.attribute::<TypeNode>("typeName").unwrap();
                let type_node = parse_variable_declaration_type(&type_name, structs);

                inner_types.push((name, type_node));
            }

            Type::Tuple(TypeTuple { types: inner_types })
        }
        TypeNodeType::ElementaryTypeName => {
            let name: String = typ.attribute("name").unwrap();
            let name = name.as_str();

            // Handle bytes<N>
            if name.starts_with("bytes") && name.len() > 5 {
                let size = name[5..].parse::<u16>().unwrap();
                if size >= 1 && size <= 32 {
                    return Type::FixedBytes(size);
                }
            }

            // handle int<bytes> and uint<bytes>
            if name.starts_with("int") {
                let size = name[3..].parse::<u16>().unwrap();
                return Type::Int(Some(size));
            } else if name.starts_with("uint") {
                let size = name[4..].parse::<u16>().unwrap();
                return Type::Uint(Some(size));
            }

            // handle address and bool
            match name {
                "address" => Type::Address,
                "bool" => Type::Bool,
                "bytes" => Type::Bytes,
                "string" => Type::String,
                _ => {
                    panic!("unknown type name: {}", name);
                }
            }
        }
        TypeNodeType::Mapping => {
            let key_type = typ.attribute("keyType").unwrap();
            let value_type = typ.attribute("valueType").unwrap();

            let one = parse_variable_declaration_type(&key_type, structs);
            let two = parse_variable_declaration_type(&value_type, structs);

            Type::Mapping(TypeMapping {
                key: Box::new(one),
                value: Box::new(two),
            })
        }
        TypeNodeType::ArrayTypeName => {
            let base_type = typ.attribute("baseType").unwrap();
            let inner_type = parse_variable_declaration_type(&base_type, structs);

            let length = match typ.attribute::<TypeNode>("length") {
                Some(node) => {
                    let value = node.attribute::<String>("value").unwrap();
                    Some(value.parse::<u16>().unwrap())
                }
                _ => None,
            };

            Type::Array(TypeArray {
                ty: Box::new(inner_type),
                size: length,
            })
        }
        TypeNodeType::Literal => {
            unreachable!()
        }
    }
}

fn extract_state_variables(ast: &Ast) -> Vec<(String, Type)> {
    let mut structs = HashMap::new();

    // loop over the structs first
    for node in ast.nodes.iter() {
        for child in node.nodes.iter() {
            if child.node_type == NodeType::StructDefinition {
                structs.insert(child.id.unwrap(), child.clone());
            }
        }
    }

    let mut variables = Vec::new();

    // loop over the variables then
    for node in ast.nodes.iter() {
        for child in node.nodes.iter() {
            if child.node_type == NodeType::VariableDeclaration {
                let typ: TypeNode =
                    serde_json::from_value(child.other.get("typeName").unwrap().clone()).unwrap();

                let ty = parse_variable_declaration_type(&typ, &structs);
                let name = child.attribute::<String>("name").unwrap();

                variables.push((name, ty));
            }
        }
    }

    variables
}

/// Recursively visits all nodes of a specified type in the AST and calls the provided callback
pub fn visit_nodes<F>(node: &Node, target_type: NodeType, callback: &mut F)
where
    F: FnMut(&Node),
{
    // Check if current node matches target type
    if node.node_type == target_type {
        callback(node);
    }

    // Recursively visit all child nodes
    for child in &node.nodes {
        visit_nodes(child, target_type.clone(), callback);
    }

    // Some nodes may have additional children in other fields
    // For example, body nodes in function definitions
    if let Some(body) = &node.body {
        visit_nodes(&body, target_type.clone(), callback);
    }

    // check declaratison for variable declaration statements
    if let Some(declarations) = node.attribute::<Vec<Node>>("declarations") {
        for declaration in declarations {
            visit_nodes(&declaration, target_type.clone(), callback);
        }
    }

    // Check the statements mark for body of a function
    if let Some(statements) = node.attribute::<Vec<Node>>("statements") {
        for statement in statements {
            visit_nodes(&statement, target_type.clone(), callback);
        }
    }
}

/// Helper function that starts the traversal from the AST root
pub fn visit_ast_nodes<F>(ast: &Ast, target_type: NodeType, mut callback: F)
where
    F: FnMut(&Node),
{
    for node in &ast.nodes {
        visit_nodes(node, target_type.clone(), &mut callback);
    }
}

#[derive(Clone, Debug)]
enum Location {
    Storage,
    Memory,
    Stack,
}

#[derive(Clone, Debug)]
struct Variable {
    name: String,
    typ: Type,
    location: Location,
}

fn extract_non_state_variables(ast: &Ast, bytecode: CompactBytecode) -> HashMap<usize, Variable> {
    let mut structs = HashMap::new();
    let mut variables = HashMap::new();

    let source_map = parse(&bytecode.clone().source_map.unwrap()).unwrap();
    let pc_ic_map = IcPcMap::new(&bytecode.bytes().unwrap());

    // Collect all struct definitions
    visit_ast_nodes(ast, NodeType::StructDefinition, |node| {
        structs.insert(node.id.unwrap(), node.clone());
    });

    // Collect state variables
    visit_ast_nodes(ast, NodeType::VariableDeclaration, |node: &Node| {
        // Check if it's a state variable (not a parameter or local var)
        let start = node.src.start as u32;
        let length = node.src.length.unwrap() as u32;

        let matches = source_map
            .iter()
            .enumerate()
            .filter(|(_, elem)| elem.offset() == start && elem.length() == length)
            .collect::<Vec<_>>();

        let typ: TypeNode =
            serde_json::from_value(node.other.get("typeName").unwrap().clone()).unwrap();

        let ty = parse_variable_declaration_type(&typ, &structs);
        let name = node.attribute::<String>("name").unwrap();

        let location = match node
            .attribute::<String>("storageLocation")
            .unwrap()
            .as_str()
        {
            "storage" => Location::Storage,
            "memory" => Location::Memory,
            "default" => Location::Stack,
            _ => unreachable!(),
        };

        // there can only be one match
        // For types::Array I had to use first, for string there was only one match
        let m = matches.first().unwrap();
        let pc = pc_ic_map.get(m.0).unwrap();

        variables.insert(
            pc,
            Variable {
                name,
                typ: ty,
                location,
            },
        );
    });

    variables
}

#[derive(Serialize)]
pub struct GeneratedContract {
    pub source: String,
    pub values: JsonValue,
}

#[derive(Clone, Debug)]
struct Assignment {
    variable: Variable,
    stack_index: usize,
}

fn resolve_memory_assignment(typ: Type, offset_bytes: usize, memory: Bytes) -> serde_json::Value {
    match typ {
        Type::String | Type::Bytes => {
            // Read the length from the first 32 bytes at the offset
            if offset_bytes + 32 > memory.len() {
                return serde_json::Value::Null;
            }

            let length_bytes = &memory[offset_bytes..offset_bytes + 32];
            let length = U256::from_be_bytes::<32>(length_bytes.try_into().unwrap());
            let length_usize = length.as_limbs()[0] as usize;

            // Read the actual string data that follows the length
            let data_offset = offset_bytes + 32;
            if data_offset + length_usize > memory.len() {
                return serde_json::Value::Null;
            }

            let string_bytes = memory[data_offset..data_offset + length_usize].to_vec();
            typ.decode_bytes(&string_bytes).unwrap()
        }
        Type::Array(TypeArray { ty, size }) => {
            // For fixed-size arrays, use the offset directly
            // For dynamic arrays, the offset points to the length
            let (length, data_offset) = match size {
                Some(fixed_size) => (fixed_size as usize, offset_bytes), // Use offset directly
                None => {
                    // For dynamic arrays, read length from first 32 bytes
                    if offset_bytes + 32 > memory.len() {
                        return serde_json::Value::Null;
                    }
                    let length_bytes = &memory[offset_bytes..offset_bytes + 32];
                    let length = U256::from_be_bytes::<32>(length_bytes.try_into().unwrap());
                    (length.as_limbs()[0] as usize, offset_bytes + 32)
                }
            };

            let mut values = Vec::new();
            let mut current_offset = data_offset;

            // Process each array element
            for _i in 0..length {
                let val_offset = match *ty {
                    Type::Address
                    | Type::Bool
                    | Type::Uint(_)
                    | Type::Int(_)
                    | Type::FixedBytes(_) => current_offset,
                    _ => {
                        // there is a dual reference here for internal elements
                        let val = &memory[current_offset..current_offset + 32];
                        let val = U256::from_be_bytes::<32>(val.try_into().unwrap());
                        let val = val.as_limbs()[0] as usize;
                        val
                    }
                };

                let element_value =
                    resolve_memory_assignment((*ty).clone(), val_offset, memory.clone());

                values.push(element_value);
                current_offset += 32;
            }

            serde_json::Value::Array(values)
        }
        Type::Tuple(tuple) => {
            let mut map = serde_json::Map::new();
            let mut current_offset = offset_bytes;

            for (name, ty) in tuple.types {
                let val_offset = match ty {
                    Type::Address
                    | Type::Bool
                    | Type::Uint(_)
                    | Type::Int(_)
                    | Type::FixedBytes(_) => current_offset,
                    _ => {
                        // there is a dual reference here for internal elements
                        let val = &memory[current_offset..current_offset + 32];
                        let val = U256::from_be_bytes::<32>(val.try_into().unwrap());
                        let val = val.as_limbs()[0] as usize;
                        val
                    }
                };

                let value = resolve_memory_assignment(ty.clone(), val_offset, memory.clone());
                map.insert(name, value);

                // For all types except Mapping, advance the offset by 32 bytes
                match ty {
                    Type::Mapping(_) => {}
                    _ => current_offset += 32,
                }
            }

            serde_json::Value::Object(map)
        }
        _ => {
            // match basic elements
            // get the slot for 32 bytes and size items from the right
            let value_bytes = &memory[offset_bytes..offset_bytes + 32];
            let typ_size = typ.get_bytes();

            // fixed bytes pads to the left and the other elements pads to the right
            let value_bytes = match typ {
                Type::FixedBytes(_) => value_bytes[0..typ_size as usize].to_vec(),
                _ => value_bytes[32 - typ_size as usize..].to_vec(),
            };

            typ.decode_bytes(&value_bytes).unwrap()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum StackType {
    Address,
    Bool,
    Uint(u16),
    Int(u16),
    FixedBytes(u16),
}

impl StackType {
    fn arbitrary_type(u: &mut Unstructured) -> arbitrary::Result<Type> {
        // Get number of elements (1-5)
        let num_elements = u.int_in_range(1..=5)?;

        // Generate that many stack types and convert to Type
        let mut types = Vec::new();
        for i in 0..num_elements {
            let stack_type = StackType::arbitrary(u)?;
            let typ = Type::from(stack_type);
            types.push((format!("arg_{}", i), typ));
        }

        Ok(Type::Tuple(TypeTuple { types }))
    }
}

impl<'a> Arbitrary<'a> for StackType {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        match u.choose(&[0, 1, 2, 3, 4])? {
            0 => Ok(StackType::Address),
            1 => Ok(StackType::Bool),
            2 => {
                // Generate uint sizes: 8, 16, 32, 64, 128, 256
                let sizes = [8u16, 16, 32, 64, 128, 256];
                let size = *u.choose(&sizes)?;
                Ok(StackType::Uint(size))
            }
            3 => {
                // Generate int sizes: 8, 16, 32, 64, 128, 256
                let sizes = [8u16, 16, 32, 64, 128, 256];
                let size = *u.choose(&sizes)?;
                Ok(StackType::Int(size))
            }
            4 => {
                // Generate fixed bytes sizes between 1 and 32
                let size = u.int_in_range(1..=32)?;
                Ok(StackType::FixedBytes(size))
            }
            _ => unreachable!(),
        }
    }
}

impl From<StackType> for Type {
    fn from(stack_type: StackType) -> Self {
        match stack_type {
            StackType::Address => Type::Address,
            StackType::Bool => Type::Bool,
            StackType::Uint(size) => Type::Uint(Some(size)),
            StackType::Int(size) => Type::Int(Some(size)),
            StackType::FixedBytes(size) => Type::FixedBytes(size),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CompilerOutput {
    /// The creation bytecode
    #[serde(rename = "bin")]
    pub bin: Bytes,

    #[serde(rename = "srcmap")]
    pub srcmap: String,

    /// The runtime bytecode
    #[serde(rename = "bin-runtime")]
    pub bin_runtime: Bytes,

    #[serde(rename = "srcmap-runtime")]
    pub srcmap_runtime: String,

    pub ast: Ast,
}

impl CompilerOutput {
    pub fn compact_deployed_bytecode(&self) -> CompactBytecode {
        CompactBytecode {
            object: BytecodeObject::Bytecode(self.bin.clone()),
            source_map: Some(self.srcmap_runtime.clone()),
            link_references: Default::default(),
        }
    }

    pub fn compact_bytecode(&self) -> CompactBytecode {
        CompactBytecode {
            object: BytecodeObject::Bytecode(self.bin_runtime.clone()),
            source_map: Some(self.srcmap_runtime.clone()),
            link_references: Default::default(),
        }
    }

    fn bytecode(&self) -> Bytes {
        self.bin.clone()
    }
}

#[derive(Debug, Deserialize)]
pub struct ContractOutput {
    /// The creation bytecode
    pub bin: Bytes,
    /// The runtime bytecode
    #[serde(rename = "bin-runtime")]
    pub bin_runtime: Bytes,

    #[serde(rename = "srcmap")]
    pub srcmap: String,

    #[serde(rename = "srcmap-runtime")]
    pub srcmap_runtime: String,
}

#[derive(Debug, Deserialize)]
pub struct SourceOutput {
    #[serde(rename = "AST")]
    pub ast: Ast,
}

pub fn compile_contract(source: &str) -> eyre::Result<CompilerOutput> {
    let mut cmd = Command::new("solc")
        .args([
            "--combined-json",
            "ast,bin,bin-runtime,srcmap,srcmap-runtime", // Get AST and both creation/runtime bytecode
            "--pretty-json",                             // Make the output readable
            "-",                                         // Read from stdin
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Write the source to stdin
    if let Some(mut stdin) = cmd.stdin.take() {
        stdin.write_all(source.as_bytes())?;
    }

    let output = cmd.wait_with_output()?;

    if output.status.success() {
        let json_output = String::from_utf8(output.stdout)
            .map_err(|e| eyre::eyre!("Failed to parse solc output as UTF-8: {}", e))?;

        {
            // Create a unique test directory
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();

            let test_dir = format!("debug/test-{}", timestamp);
            std::fs::create_dir_all(&test_dir)?;

            // Write the Solidity source
            std::fs::write(format!("{}/contract.sol", test_dir), source)?;

            // Write the compiler output
            std::fs::write(format!("{}/solc_output.json", test_dir), &json_output)?;
        }

        #[derive(Debug, Deserialize)]
        struct FullCompilerOutput {
            contracts: HashMap<String, ContractOutput>,
            sources: HashMap<String, SourceOutput>,
        }

        let parsed: FullCompilerOutput = serde_json::from_str(&json_output)
            .map_err(|e| eyre::eyre!("Failed to parse compiler output: {}", e))?;

        // there should be a single contract compiled
        let contract = parsed.contracts.values().next().unwrap();
        let source = parsed.sources.values().next().unwrap();

        let output = CompilerOutput {
            bin: contract.bin.clone(),
            bin_runtime: contract.bin_runtime.clone(),
            srcmap_runtime: contract.srcmap_runtime.clone(),
            srcmap: contract.srcmap.clone(),
            ast: source.ast.clone(),
        };
        Ok(output)
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Solidity compilation failed: {}", error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_provider::fillers::{
        BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, WalletFiller,
    };
    use alloy_provider::network::{Ethereum, EthereumWallet, TransactionBuilder};
    use alloy_provider::{ext::DebugApi, Provider, ProviderBuilder};
    use alloy_provider::{Identity, RootProvider};
    use alloy_rpc_types::TransactionRequest;
    use alloy_rpc_types_trace::geth::{
        DefaultFrame, GethDebugTracingOptions, GethDefaultTracingOptions,
    };
    use alloy_sol_types::sol;
    use alloy_transport::BoxTransport;
    use arbitrary::Unstructured;
    use foundry_compilers::artifacts::BytecodeObject;
    use std::io::Write;
    use std::process::{Command, Stdio};

    type AnvilProvider = FillProvider<
        JoinFill<
            JoinFill<
                Identity,
                JoinFill<GasFiller, JoinFill<BlobGasFiller, JoinFill<NonceFiller, ChainIdFiller>>>,
            >,
            WalletFiller<EthereumWallet>,
        >,
        alloy_provider::layers::AnvilProvider<RootProvider<BoxTransport>, BoxTransport>,
        BoxTransport,
        Ethereum,
    >;

    sol! {
        #[allow(missing_docs)]
        #[sol(rpc)]
        contract Example1 {
            function setup() public {}
        }
    }

    struct TestHarness {
        provider: AnvilProvider,
    }

    struct DeployResult {
        frame: DefaultFrame,
        contract: CompilerOutput,
    }

    impl DeployResult {
        fn retrieve_storage(&self) -> JsonValue {
            let state_variables = extract_state_variables(&self.contract.ast);

            let storage_slots = self
                .frame
                .struct_logs
                .iter()
                .rev()
                .find(|step| step.op == "SSTORE" && step.storage.is_some())
                .and_then(|step| step.storage.clone())
                .unwrap_or_default()
                .into_iter()
                .collect::<HashMap<_, _>>();

            let state_reference = StateReference::new(storage_slots);
            let result = state_reference.resolve_vars(state_variables);

            result
        }

        fn retrieve_memory(&self) -> JsonValue {
            let variables =
                extract_non_state_variables(&self.contract.ast, self.contract.compact_bytecode());

            let mut assignments = Vec::new();

            // loop over the result and find the pc that belong to variable declarations
            for step in self.frame.struct_logs.clone() {
                let pc = step.pc as usize;
                let stack = step.stack.unwrap();

                if let Some(var) = variables.get(&pc) {
                    assignments.push(Assignment {
                        variable: var.clone(),
                        stack_index: stack.len() - 1,
                    });
                }
            }

            // find how many variables we have in the stack, check assigments and get the largest
            // value in the stack_index field
            let max_stack_index = assignments.iter().map(|a| a.stack_index).max().unwrap();

            let last_mstore_index = self
                .frame
                .struct_logs
                .iter()
                .enumerate()
                .rev()
                .find(|(_, step)| {
                    step.stack
                        .as_ref()
                        .map(|stack| stack.len() > max_stack_index + 1)
                        .unwrap_or(false)
                })
                .map(|(i, _)| i)
                .unwrap();

            // if we query for the last msore, the memory has not being set
            // yet so we miss part of the memory allocation.
            // not sure why but it is 3 allocations after the last mstore that
            // all the data is usually there.
            let last_mstore = self.frame.struct_logs[last_mstore_index].clone();

            // get the latest step to get the memory snapshot
            let memory = last_mstore.memory.as_ref().unwrap();

            // convert memory into a vec<u8>
            let memory = memory
                .iter()
                .map(|s| Bytes::from_str(s).unwrap())
                .collect::<Vec<Bytes>>();

            // Concatenate all bytes into a single Bytes object
            let memory = memory.iter().fold(Bytes::new(), |mut acc, bytes| {
                let mut new_bytes = vec![0u8; acc.len() + bytes.len()];
                new_bytes[..acc.len()].copy_from_slice(&acc);
                new_bytes[acc.len()..].copy_from_slice(bytes);
                Bytes::from(new_bytes)
            });

            // resolve each assignment
            let mut result = serde_json::Map::new();
            for assignment in assignments {
                let name = assignment.variable.name.clone();

                let offset = last_mstore
                    .stack
                    .as_ref()
                    .unwrap()
                    .get(assignment.stack_index + 1)
                    .unwrap();

                match assignment.variable.location {
                    Location::Stack => {
                        let typ = assignment.variable.typ.clone();

                        // convert the location into bytes and decode the simple type
                        let value_bytes = offset.to_be_bytes::<32>();
                        let typ_size = typ.get_bytes();

                        // fixed bytes pads to the left and the other elements pads to the right
                        let value_bytes = match typ {
                            Type::FixedBytes(_) => value_bytes[0..typ_size as usize].to_vec(),
                            _ => value_bytes[32 - typ_size as usize..].to_vec(),
                        };

                        let value = assignment.variable.typ.decode_bytes(&value_bytes).unwrap();
                        result.insert(name, value);
                    }
                    Location::Memory => {
                        // convert the stack location into a usize index to memory
                        let offset_bytes = offset.as_limbs()[0] as usize;

                        result.insert(
                            name,
                            resolve_memory_assignment(
                                assignment.variable.typ,
                                offset_bytes,
                                memory.clone(),
                            ),
                        );
                    }
                    _ => {}
                }
            }

            serde_json::Value::Object(result)
        }
    }

    #[derive(Debug)]
    enum DeployError {
        RecoverableError(String),
        FatalError(String),
    }

    impl TestHarness {
        const RECOVERABLE_ERRORS: &'static [&'static str] = &[
            "max initcode size exceeded",
            "EVM error CreateContractSizeLimit",
            "array out-of-bounds",
        ];

        fn new() -> Self {
            let provider = ProviderBuilder::new()
                .with_recommended_fillers()
                .on_anvil_with_wallet_and_config(|anvil| anvil.arg("--steps-tracing"));

            TestHarness { provider: provider }
        }

        fn is_recoverable_error(err: &str) -> bool {
            Self::RECOVERABLE_ERRORS.iter().any(|&e| err.contains(e))
        }

        async fn deploy(&self, source: &str) -> Result<DeployResult, DeployError> {
            let provider = self.provider.clone();
            let contract_artifact = compile_contract(&source).unwrap();

            println!("source {}", source);

            // Deploy the `Counter` contract from bytecode at runtime.
            let tx = TransactionRequest::default().with_deploy_code(contract_artifact.bytecode());

            // Deploy the contract.
            let deploy_pending_tx = match provider.send_transaction(tx).await {
                Ok(pending_tx) => pending_tx,
                Err(err) => {
                    let err_str = err.to_string();
                    return if Self::is_recoverable_error(&err_str) {
                        Err(DeployError::RecoverableError(err_str))
                    } else {
                        Err(DeployError::FatalError(err_str))
                    };
                }
            };

            let receipt = deploy_pending_tx.get_receipt().await.unwrap();
            let contract_address = receipt.contract_address.unwrap();

            let contract = Example1::new(contract_address, &provider);
            let builder = contract.setup();

            let pending_tx = match builder.send().await {
                Ok(pending_tx) => pending_tx,
                Err(err) => {
                    let err_str = err.to_string();
                    return if Self::is_recoverable_error(&err_str) {
                        Err(DeployError::RecoverableError(err_str))
                    } else {
                        Err(DeployError::FatalError(err_str))
                    };
                }
            };

            let tx_hash = pending_tx.watch().await.unwrap();

            let call_options = GethDebugTracingOptions {
                tracer: None,
                config: GethDefaultTracingOptions {
                    enable_memory: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            };
            let result = provider
                .debug_trace_transaction(tx_hash, call_options)
                .await
                .unwrap();

            let frame = result.try_into_default_frame().unwrap();
            Ok(DeployResult {
                frame,
                contract: contract_artifact,
            })
        }
    }

    #[tokio::test]
    async fn test_storage_fuzz() {
        let harness = TestHarness::new();

        let num = var("NUM_TESTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);

        for i in 0..num {
            println!("generting {} {}", i, num);

            let mut data = vec![0u8; 1000];
            getrandom::getrandom(&mut data).unwrap();

            let mut u = Unstructured::new(&data);
            let typ = TypeTuple::arbitrary(&mut u).unwrap();

            let artifact = ContractGenerator::build_storage(Type::Tuple(typ), &mut u);
            let result = match harness.deploy(&artifact.source).await {
                Ok(result) => result,
                Err(DeployError::RecoverableError(_)) => continue,
                Err(DeployError::FatalError(err)) => {
                    std::panic!("bad {:?}", err);
                }
            };

            let result = result.retrieve_storage();
            assert_eq!(result, artifact.values);
        }
    }

    #[tokio::test]
    async fn test_storage_types() {
        let typ = TypeTuple {
            types: vec![
                ("arg_0".to_string(), Type::String),
                ("arg_1".to_string(), Type::Bytes),
                (
                    "arg_2".to_string(),
                    Type::Array(TypeArray {
                        ty: Box::new(Type::Bool),
                        size: None,
                    }),
                ),
                ("arg_3".to_string(), Type::Uint(Some(128))),
                (
                    "a".to_string(),
                    Type::Tuple(TypeTuple {
                        types: vec![
                            ("a".to_string(), Type::Address),
                            (
                                "b".to_string(),
                                Type::Tuple(TypeTuple {
                                    types: vec![
                                        ("a".to_string(), Type::Address),
                                        ("b".to_string(), Type::Bytes),
                                    ],
                                }),
                            ),
                        ],
                    }),
                ),
                ("b".to_string(), Type::Bytes),
                ("b1".to_string(), Type::Uint(Some(32))),
                ("c".to_string(), Type::String),
                ("c1".to_string(), Type::Address),
                (
                    "e".to_string(),
                    Type::Array(TypeArray {
                        ty: Box::new(Type::Array(TypeArray {
                            ty: Box::new(Type::Bytes),
                            size: Some(2),
                        })),
                        size: Some(2),
                    }),
                ),
                (
                    "e".to_string(),
                    Type::Array(TypeArray {
                        ty: Box::new(Type::Array(TypeArray {
                            ty: Box::new(Type::Bytes),
                            size: Some(2),
                        })),
                        size: Some(2),
                    }),
                ),
                (
                    "f".to_string(),
                    Type::Array(TypeArray {
                        ty: Box::new(Type::Address),
                        size: None,
                    }),
                ),
                (
                    "f2".to_string(),
                    Type::Array(TypeArray {
                        ty: Box::new(Type::Bytes),
                        size: None,
                    }),
                ),
                (
                    "f3".to_string(),
                    Type::Array(TypeArray {
                        ty: Box::new(Type::Array(TypeArray {
                            ty: Box::new(Type::Bytes),
                            size: None,
                        })),
                        size: None,
                    }),
                ),
                ("g".to_string(), Type::FixedBytes(10)),
                ("a".to_string(), Type::Bool),
            ],
        };

        let mut data = vec![0u8; 1000000];
        for i in 0..data.len() {
            data[i] = (i + 1) as u8;
        }

        let mut u = Unstructured::new(&data);
        let artifact = ContractGenerator::build_storage(Type::Tuple(typ), &mut u);

        let harness = TestHarness::new();
        let result = harness.deploy(&artifact.source).await.unwrap();

        let values = result.retrieve_storage();
        assert_eq!(values, artifact.values);
    }

    #[tokio::test]
    async fn test_memory_fuzz() {
        let harness = TestHarness::new();

        let num = var("NUM_TESTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);

        for i in 0..num {
            println!("generting {} {}", i, num);

            let mut data = vec![0u8; 1000];
            getrandom::getrandom(&mut data).unwrap();

            let mut u = Unstructured::new(&data);
            let typ = MemoryType::arbitrary_type(&mut u).unwrap();

            let mut u = Unstructured::new(&data);
            let artifact = ContractGenerator::build_memory(typ.clone(), &mut u);

            let result = match harness.deploy(&artifact.source).await {
                Ok(result) => result,
                Err(DeployError::RecoverableError(_)) => continue,
                Err(DeployError::FatalError(err)) => {
                    std::panic!("bad {:?}", err);
                }
            };

            let result = result.retrieve_memory();
            assert_eq!(result, artifact.values);
        }
    }

    #[tokio::test]
    async fn test_stack_fuzz() {
        let harness = TestHarness::new();

        let num = var("NUM_TESTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);

        for i in 0..num {
            println!("generating {} {}", i, num);

            let mut data = vec![0u8; 1000];
            getrandom::getrandom(&mut data).unwrap();

            let mut u = Unstructured::new(&data);
            let typ = StackType::arbitrary_type(&mut u).unwrap();

            let mut u = Unstructured::new(&data);
            let artifact = ContractGenerator::build_stack(typ.clone(), &mut u);

            let result = match harness.deploy(&artifact.source).await {
                Ok(result) => result,
                Err(DeployError::RecoverableError(_)) => continue,
                Err(DeployError::FatalError(err)) => {
                    std::panic!("Fatal error: {:?}", err);
                }
            };

            let result = result.retrieve_memory();
            println!("{:?}", result);
            println!("{:?}", artifact.values);

            // assert_eq!(result, artifact.values);
        }
    }
}
