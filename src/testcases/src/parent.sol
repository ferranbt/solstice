// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

contract Parent {
    uint256 parent_value_2;

    function set_parent_value_1(uint256 new_parent_value) public {
        parent_value_2 = new_parent_value;
    } 
}

contract ParentWithConstructor {
    uint256 parent_value_2;

    constructor() {
        parent_value_2 = 10;
    }
}
