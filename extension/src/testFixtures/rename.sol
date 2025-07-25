// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.0;

contract Rename {
    uint256 public value1;

    function setOtherValue(uint256 value2) public {
        value1 += value2;
    }
}
