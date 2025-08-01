// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";

contract ParamsTest is Test {
    uint256 value;

    function test_params() external {
        call_one(1111, 2222, 3333);
    }

    // TODO: keeping this one here for now but the current test framework does not support
    // checking the state snapshot other than at the end of the test function, and since our tests
    // functions cannot have parameters, we cannot test in integration function parameters (for now).
    function call_one(uint256 val1, uint256 val2, uint256 val3) public pure returns (uint256, uint256, uint256) {
        return (val1 + val2 + val3, val1 * val2 * val3, val1 - val2 - val3);
    }
}
