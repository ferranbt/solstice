// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

contract Parent {
    uint256 parent_value;

    function set_parent_value(uint256 new_parent_value) public {
        parent_value = new_parent_value;
    } 
}

contract Example {
    uint256 value;

    constructor() {
        value = 10;
    }

    function main() public {
        if (value == 1) {
            simple_call();
        } else {
            uint256 x = simple_call();
        }

        for (uint256 i = 0; i < 3; i++) {
            value += simple_call();
        }
    }

    function simple_call() public pure returns (uint256) {
        return 1;
    }
}
