use regex::Regex;

#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    pub name: String,
    pub line: usize,
}

pub struct ContractParser {
    contract_regex: Regex,
}

impl ContractParser {
    pub fn new() -> Self {
        Self {
            // Match: contract MyContract, abstract contract MyContract
            // Handles optional abstract keyword, contract name, optional inheritance, optional opening brace
            contract_regex: Regex::new(
                r"(?m)^\s*(?:(abstract)\s+)?contract\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:is\s+[^{]*)?(?:\s*\{)?"
            ).unwrap(),
        }
    }

    pub fn extract_contracts(&self, content: &str) -> Vec<ExtractedSymbol> {
        let mut contracts = Vec::new();

        for capture in self.contract_regex.captures_iter(content) {
            let full_match = capture.get(0).unwrap();
            let contract_name = capture.get(2).unwrap(); // Contract name

            // Calculate line number (0-indexed)
            let line_number = content[..full_match.start()].lines().count();

            contracts.push(ExtractedSymbol {
                name: contract_name.as_str().to_string(),
                line: line_number,
            });
        }

        contracts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_parser() -> ContractParser {
        ContractParser::new()
    }

    #[test]
    fn test_simple_contract() {
        let parser = test_parser();
        let content = "contract MyContract {\n    uint256 public value;\n}";

        let contracts = parser.extract_contracts(content);

        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].name, "MyContract");
        assert_eq!(contracts[0].line, 0);
    }

    #[test]
    fn test_abstract_contract() {
        let parser = test_parser();
        let content =
            "abstract contract AbstractToken {\n    function transfer() public virtual;\n}";

        let contracts = parser.extract_contracts(content);

        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].name, "AbstractToken");
    }

    #[test]
    fn test_contract_with_inheritance() {
        let parser = test_parser();
        let content = "contract MyToken is ERC20, Ownable {\n    constructor() {}\n}";

        let contracts = parser.extract_contracts(content);

        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].name, "MyToken");
    }

    #[test]
    fn test_multiple_contracts() {
        let parser = test_parser();
        let content = r#"
contract FirstContract {
    uint256 value;
}

abstract contract SecondContract {
    function doSomething() public virtual;
}

contract ThirdContract is FirstContract {
    constructor() {}
}
"#;

        let contracts = parser.extract_contracts(content);

        assert_eq!(contracts.len(), 3);
        assert_eq!(contracts[0].name, "FirstContract");
        assert_eq!(contracts[1].name, "SecondContract");
        assert_eq!(contracts[2].name, "ThirdContract");
    }

    #[test]
    fn test_contracts_with_comments() {
        let parser = test_parser();
        let content = r#"
// This is a comment about the contract
contract MyContract { // inline comment
    uint256 value;
}

/*
 * Multi-line comment
 * describing another contract
 */
contract AnotherContract {
    /* inline multi-line */ uint256 balance;
}
"#;

        let contracts = parser.extract_contracts(content);

        assert_eq!(contracts.len(), 2);
        assert_eq!(contracts[0].name, "MyContract");
        assert_eq!(contracts[1].name, "AnotherContract");
    }

    #[test]
    fn test_indented_contracts() {
        let parser = test_parser();
        let content = r#"
    contract IndentedContract {
        uint256 value;
    }

        abstract contract DeeplyIndented {
            function test() public virtual;
        }
"#;

        let contracts = parser.extract_contracts(content);

        assert_eq!(contracts.len(), 2);
        assert_eq!(contracts[0].name, "IndentedContract");
        assert_eq!(contracts[1].name, "DeeplyIndented");
    }

    #[test]
    fn test_contract_without_braces() {
        let parser = test_parser();
        let content = "contract SimpleContract";

        let contracts = parser.extract_contracts(content);

        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].name, "SimpleContract");
    }

    #[test]
    fn test_line_numbers() {
        let parser = test_parser();
        let content = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract FirstContract {
    uint256 value;
}

contract SecondContract {
    uint256 balance;
}"#;

        let contracts = parser.extract_contracts(content);

        assert_eq!(contracts.len(), 2);
        assert_eq!(contracts[0].line, 2); // FirstContract on line 3 (0-indexed line 2)
        assert_eq!(contracts[1].line, 6); // SecondContract on line 7 (0-indexed line 6)
    }
}
