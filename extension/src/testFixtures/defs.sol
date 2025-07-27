// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.0;

contract Definitions {
    uint256 public value1;

    struct Val {
        uint256 value2;
    }

    function setValue(uint256 value2, Val memory value) public {
        value1 += value2;
        value1 += value.value2;
        value.value2 += value2;
    }

    function getValue() public view returns (uint256) {
        return value1;
    }
}
