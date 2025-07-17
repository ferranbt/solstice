// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";

enum AA0 { A, B }

contract TypesTest is Test {
    enum AA1 { A, B }

    AA0 public arg_0;
    AA1 public arg_1;

    function test_types() external {
        arg_0 = AA0.A;
        arg_1 = AA1.A;
    }
}
