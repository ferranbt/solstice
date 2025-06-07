// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

contract Example {
    uint256 value;

    constructor(uint256 val) {
        value = val;
    }

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

    modifier simpleModifier() {
        require(value > 0, "Value must be greater than 0");
        _;
        value = value + 1;
    }

    function simple_with_modifier() public simpleModifier {
        value = 5;
    }
}
