// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";

contract CreateTest is Test {
    function test_if_else() external {
        if_else(0);
        if_else(1);
        if_else_if(0);
        if_else_if(1);
        if_else_if(2);
    }

    function if_else(uint256 val) internal returns (uint256) {
        if (val == 0) {
            return 0;
        } else {
            return 1;
        }
    }

    function if_else_if(uint256 val) internal returns (uint256) {
        if (val == 0) {
            return 1;
        } else if (val == 1) {
            return 2;
        } else {
            return 3;
        }
    }
}
