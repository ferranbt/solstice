// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Example} from "../src/example1.sol";

contract FooTest is Test {
    uint256 value;

    function test_main() external simpleModifier() {
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
}
