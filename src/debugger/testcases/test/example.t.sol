// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Example, ExampleTwoParents, Struct1} from "../src/example1.sol";

contract FooTest is Test {
    uint256 value;

    function test_main() external simpleModifier {
        Example example = new Example();
        value += simple_call(1);
    }

    function simple_call(uint256 val) public pure returns (uint256) {
        return val + 1;
    }

    modifier simpleModifier() {
        require(value == 0, "Value must be greater than 0");
        _;
        value = value + 1;
    }

    function test_multiple_calls() external {
        for (uint256 i = 0; i < 3; i++) {
            value += simple_call(1);
        }
    }

    function test_contract_calls() external {
        Example example = new Example();
        example.set_parent_value(1);
        example.set_parent_value_1(1);
    }

    function test_double_parent() external {
        ExampleTwoParents example = new ExampleTwoParents();
    }

    function test_with_struct_value() external {
        Struct1 memory s = Struct1({value: 123});
        value = 3;
        value = 4;
    }
}
