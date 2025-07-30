use regex::Replacer;
use tower_lsp::lsp_types::*;

/// Efficiently tracks and applies document changes to positions
#[derive(Debug)]
pub struct PositionTracker {
    /// Line adjustments: (change_line, change_char, net_line_change)
    line_adjustments: Vec<(u32, u32, i32)>,
    /// Character adjustments for same-line changes: (line, after_char, net_char_change)
    char_adjustments: Vec<(u32, u32, i32)>,
    // Store original replacements for complex cases
    replacements: Vec<(Range, String)>,
}

impl PositionTracker {
    /// Create a new tracker from a sequence of document changes
    pub fn new(changes: &[TextDocumentContentChangeEvent]) -> Self {
        let mut line_adjustments = Vec::new();
        let mut char_adjustments = Vec::new();
        let mut replacements = Vec::new();

        for change in changes {
            if let Some(range) = &change.range {
                replacements.push((range.clone(), change.text.clone()));

                // Calculate line changes
                let lines_added = change.text.matches('\n').count() as i32;
                let lines_removed = (range.end.line - range.start.line) as i32;
                let net_line_change = lines_added - lines_removed;

                if net_line_change != 0 {
                    line_adjustments.push((
                        range.start.line,
                        range.start.character,
                        net_line_change,
                    ));
                }

                // Calculate character changes (same-line only, and only if no newlines are involved)
                if range.start.line == range.end.line && !change.text.contains('\n') {
                    let chars_added = change.text.len() as i32;
                    let chars_removed = (range.end.character - range.start.character) as i32;
                    let net_char_change = chars_added - chars_removed;

                    if net_char_change != 0 {
                        char_adjustments.push((
                            range.start.line,
                            range.end.character,
                            net_char_change,
                        ));
                    }
                }
            }
        }

        // Sort by position for efficient lookup
        line_adjustments.sort_by_key(|(line, char, _)| (*line, *char));
        char_adjustments.sort_by_key(|(line, char, _)| (*line, *char));

        Self {
            line_adjustments,
            char_adjustments,
            replacements,
        }
    }

    /// Adjust a single position based on the tracked changes
    pub fn adjust_position(&self, position: Position) -> Option<Position> {
        let mut new_position = position;

        // Apply line adjustments
        let mut cumulative_line_change = 0i32;
        for &(change_line, change_char, line_change) in &self.line_adjustments {
            if line_change < 0 {
                // This is a deletion
                let deletion_end_line = change_line + (-line_change) as u32;

                if position.line > deletion_end_line {
                    // Position is after the deleted range
                    cumulative_line_change += line_change;
                } else if position.line > change_line
                    || (position.line == change_line && position.character > change_char)
                {
                    // Position is within the deleted range - move it to the deletion start
                    cumulative_line_change = change_line as i32 - position.line as i32;
                }
            } else {
                // This is an insertion
                if position.line > change_line
                    || (position.line == change_line && position.character >= change_char)
                {
                    cumulative_line_change += line_change;
                }
            }
        }

        // Apply the cumulative line change
        let new_line = (new_position.line as i32 + cumulative_line_change).max(0) as u32;
        new_position.line = new_line;

        // Apply character adjustments
        if new_position.line == position.line {
            // Same line adjustments
            let mut cumulative_char_change = 0i32;
            for &(line, after_char, char_change) in &self.char_adjustments {
                if line == position.line && position.character >= after_char {
                    cumulative_char_change += char_change;
                }
            }

            let new_char = (new_position.character as i32 + cumulative_char_change).max(0) as u32;
            new_position.character = new_char;
        } else if new_position.line < position.line {
            // Position moved to an earlier line - handle cross-line replacements
            for (range, replacement_text) in &self.replacements {
                if range.start.line == new_position.line && range.end.line == position.line {
                    // This replacement merged our line
                    let chars_before_replacement = range.start.character;
                    let chars_after_removed_part = if position.character > range.end.character {
                        position.character - range.end.character
                    } else {
                        0
                    };

                    new_position.character = chars_before_replacement
                        + replacement_text.len() as u32
                        + chars_after_removed_part;
                    break;
                }
            }
        }

        // Validate the result
        if self.is_valid_position(&new_position) {
            Some(new_position)
        } else {
            None
        }
    }

    /// Adjust multiple positions efficiently
    pub fn adjust_positions(&self, positions: Vec<Position>) -> Vec<Position> {
        positions
            .into_iter()
            .filter_map(|pos| self.adjust_position(pos))
            .collect()
    }

    /// Check if a position is valid
    pub fn is_valid_position(&self, position: &Position) -> bool {
        position.line < u32::MAX && position.character < u32::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Visual test framework for Solidity code with clear actions
    #[derive(Debug)]
    pub struct SolidityTest {
        description: String,
        before_content: String,
        action: TestAction,
        after_content: String,
    }

    #[derive(Debug)]
    pub enum TestAction {
        /// Insert text at specific position
        Insert {
            line: u32,
            character: u32,
            text: String,
        },
        /// Delete text in range
        Delete {
            start_line: u32,
            start_char: u32,
            end_line: u32,
            end_char: u32,
        },
        /// Replace text in range
        Replace {
            start_line: u32,
            start_char: u32,
            end_line: u32,
            end_char: u32,
            text: String,
        },
    }

    impl SolidityTest {
        pub fn new(description: &str) -> Self {
            Self {
                description: description.to_string(),
                before_content: String::new(),
                action: TestAction::Insert {
                    line: 0,
                    character: 0,
                    text: String::new(),
                },
                after_content: String::new(),
            }
        }

        /// Set the Solidity code before the change (use → to mark hint positions)
        pub fn before(mut self, content: &str) -> Self {
            self.before_content = content.trim().to_string();
            self
        }

        /// Specify the action that will be performed
        pub fn insert_at(mut self, line: u32, character: u32, text: &str) -> Self {
            self.action = TestAction::Insert {
                line,
                character,
                text: text.to_string(),
            };
            self
        }

        pub fn delete_range(
            mut self,
            start_line: u32,
            start_char: u32,
            end_line: u32,
            end_char: u32,
        ) -> Self {
            self.action = TestAction::Delete {
                start_line,
                start_char,
                end_line,
                end_char,
            };
            self
        }

        pub fn replace_range(
            mut self,
            start_line: u32,
            start_char: u32,
            end_line: u32,
            end_char: u32,
            text: &str,
        ) -> Self {
            self.action = TestAction::Replace {
                start_line,
                start_char,
                end_line,
                end_char,
                text: text.to_string(),
            };
            self
        }

        /// Set the expected Solidity code after the change (→ markers show where hints should be)
        pub fn after(mut self, content: &str) -> Self {
            self.after_content = content.trim().to_string();
            self
        }

        /// Extract positions of → markers from document content
        fn extract_hint_positions(&self, content: &str) -> Vec<Position> {
            let mut positions = Vec::new();

            for (line_idx, line) in content.lines().enumerate() {
                for (char_idx, ch) in line.chars().enumerate() {
                    if ch == '→' {
                        positions.push(Position {
                            line: line_idx as u32,
                            character: char_idx as u32,
                        });
                    }
                }
            }

            positions
        }

        /// Convert TestAction to TextDocumentContentChangeEvent
        fn to_change_event(&self) -> TextDocumentContentChangeEvent {
            match &self.action {
                TestAction::Insert {
                    line,
                    character,
                    text,
                } => TextDocumentContentChangeEvent {
                    range: Some(Range {
                        start: Position {
                            line: *line,
                            character: *character,
                        },
                        end: Position {
                            line: *line,
                            character: *character,
                        },
                    }),
                    range_length: None,
                    text: text.clone(),
                },
                TestAction::Delete {
                    start_line,
                    start_char,
                    end_line,
                    end_char,
                } => TextDocumentContentChangeEvent {
                    range: Some(Range {
                        start: Position {
                            line: *start_line,
                            character: *start_char,
                        },
                        end: Position {
                            line: *end_line,
                            character: *end_char,
                        },
                    }),
                    range_length: None,
                    text: String::new(),
                },
                TestAction::Replace {
                    start_line,
                    start_char,
                    end_line,
                    end_char,
                    text,
                } => TextDocumentContentChangeEvent {
                    range: Some(Range {
                        start: Position {
                            line: *start_line,
                            character: *start_char,
                        },
                        end: Position {
                            line: *end_line,
                            character: *end_char,
                        },
                    }),
                    range_length: None,
                    text: text.clone(),
                },
            }
        }

        /// Run the test
        pub fn run(self) {
            println!("\n=== {} ===", self.description);

            // Show the visual before/after with line numbers
            println!("BEFORE:");
            for (i, line) in self.before_content.lines().enumerate() {
                println!("{:2}: {}", i, line);
            }

            println!("\nACTION: {:?}", self.action);

            println!("\nAFTER:");
            for (i, line) in self.after_content.lines().enumerate() {
                println!("{:2}: {}", i, line);
            }

            // Extract hint positions
            let before_positions = self.extract_hint_positions(&self.before_content);
            let expected_positions = self.extract_hint_positions(&self.after_content);

            assert_eq!(
                before_positions.len(),
                expected_positions.len(),
                "Number of hint markers must match between before and after"
            );

            // Create the position tracker
            let change = self.to_change_event();
            let tracker = PositionTracker::new(&[change]);

            // Test each hint position
            for (i, (before_pos, expected_pos)) in before_positions
                .iter()
                .zip(expected_positions.iter())
                .enumerate()
            {
                let actual_pos = tracker.adjust_position(*before_pos);
                match actual_pos {
                    Some(actual) => {
                        if actual != *expected_pos {
                            panic!(
                                "❌ Hint {} moved incorrectly!\n  Before: {:?}\n  Expected: {:?}\n  Actual: {:?}",
                                i, before_pos, expected_pos, actual
                            );
                        } else {
                            println!("✓ Hint {}: {:?} → {:?}", i, before_pos, actual);
                        }
                    }
                    None => {
                        panic!("❌ Hint {} became invalid! Before: {:?}", i, before_pos);
                    }
                }
            }

            println!("✅ Test passed!\n");
        }
    }

    #[test]
    fn test_insert_character_in_function() {
        SolidityTest::new("Insert character in function selector hint")
            .before(
                r#"
contract MyContract {
    function transfer(address to, uint256 amount) public {→
        // function body
    }
}"#,
            )
            .insert_at(1, 55, "x") // Insert on line 1, same line as the hint
            .after(
                r#"
contract MyContract {
    function transfer(address to, uint256 amount) public {x→
        // function body
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_add_newline_before_function() {
        SolidityTest::new("Add newline - function hint should move down")
            .before(
                r#"
contract MyContract {
    function mint(uint256 amount) external {→
        _mint(msg.sender, amount);
    }
}"#,
            )
            .insert_at(1, 0, "\n") // Insert newline at beginning of line 1
            .after(
                r#"
contract MyContract {

    function mint(uint256 amount) external {→
        _mint(msg.sender, amount);
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_delete_comment_line() {
        SolidityTest::new("Delete comment line - hints should move up")
            .before(
                r#"
contract Token {
    // This comment will be deleted
    function balanceOf(address owner) view returns (uint256) {→
        return balances[owner];
    }
}"#,
            )
            .delete_range(1, 0, 2, 0) // Delete line 1 (the comment line)
            .after(
                r#"
contract Token {
    function balanceOf(address owner) view returns (uint256) {→
        return balances[owner];
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_add_parameter_to_function() {
        SolidityTest::new("Add parameter - hint should move right")
            .before(
                r#"
contract MyContract {
    function approve(address spender) external {→
        allowances[msg.sender][spender] = amount;
    }
}"#,
            )
            .insert_at(1, 37, ", uint256 amount") // Insert on line 1 where the function is
            .after(
                r#"
contract MyContract {
    function approve(address spender, uint256 amount) external {→
        allowances[msg.sender][spender] = amount;
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_multiple_function_hints() {
        SolidityTest::new("Multiple functions - only affected hints should move")
            .before(
                r#"
contract ERC20 {
    function transfer(address to, uint256 amount) external {→
        _transfer(msg.sender, to, amount);
    }
    
    function approve(address spender, uint256 amount) external {→
        allowances[msg.sender][spender] = amount;
    }
}"#,
            )
            .insert_at(4, 0, "    // Added comment\n")
            .after(
                r#"
contract ERC20 {
    function transfer(address to, uint256 amount) external {→
        _transfer(msg.sender, to, amount);
    }
    // Added comment
    
    function approve(address spender, uint256 amount) external {→
        allowances[msg.sender][spender] = amount;
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_replace_function_name() {
        SolidityTest::new("Replace function name - hint should adjust position")
            .before(
                r#"
contract MyContract {
    function mint(uint256 amount) external {→
        _mint(msg.sender, amount);
    }
}"#,
            )
            .replace_range(1, 13, 1, 17, "safeMint") // Replace on line 1 where the function is
            .after(
                r#"
contract MyContract {
    function safeMint(uint256 amount) external {→
        _mint(msg.sender, amount);
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_add_modifier_to_function() {
        SolidityTest::new("Add modifier - hint should move right")
            .before(
                r#"
contract MyContract {
    function withdraw() external {→
        payable(msg.sender).transfer(address(this).balance);
    }
}"#,
            )
            .insert_at(1, 28, " onlyOwner") // Insert on line 1 where the function is
            .after(
                r#"
contract MyContract {
    function withdraw() external onlyOwner {→
        payable(msg.sender).transfer(address(this).balance);
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_multiline_function_signature() {
        SolidityTest::new("Multiline function signature change")
            .before(
                r#"
contract MyContract {
    function complexFunction(
        address user,
        uint256 amount
    ) external {→
        // function body
    }
}"#,
            )
            .insert_at(3, 20, ",\n        bytes calldata data") // Insert after "amount"
            .after(
                r#"
contract MyContract {
    function complexFunction(
        address user,
        uint256 amount,
        bytes calldata data
    ) external {→
        // function body
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_remove_multiple_line_breaks() {
        SolidityTest::new("Remove 3 line breaks - hints should move up")
            .before(
                r#"
contract Token {
    function transfer(address to, uint256 amount) external {→



    function approve(address spender, uint256 amount) external {→
        allowances[msg.sender][spender] = amount;
    }
}"#,
            )
            .delete_range(2, 66, 6, 0) // Delete from end of transfer line to start of approve line
            .after(
                r#"
contract Token {
    function transfer(address to, uint256 amount) external {→
    function approve(address spender, uint256 amount) external {→
        allowances[msg.sender][spender] = amount;
    }
}"#,
            )
            .run();
    }

    #[test]
    fn test_format_function_signature() {
        SolidityTest::new("Format function signature - remove line break")
            .before(
                r#"
contract MyContract {
    function call_with_params(uint256 a, uint256 b)
    private returns (uint256) {→
        return a + b;
    }
}"#,
            )
            .replace_range(1, 51, 2, 4, " ") // Replace from end of line 1 to start of "private" on line 2
            .after(
                r#"
contract MyContract {
    function call_with_params(uint256 a, uint256 b) private returns (uint256) {→
        return a + b;
    }
}"#,
            )
            .run();
    }
}
