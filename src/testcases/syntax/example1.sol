// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

contract Example {
    uint256 value;

    function main() public {
        if (value == 1) {
            simple_call(1);
        } else {
            uint256 x = simple_call(2);
        }

        for (uint256 i = 0; i < 3; i++) {
            value += simple_call(3);
        }
    }

    function simple_call(uint256 val) public pure returns (uint256) {
        return val + 1;
    }
}
