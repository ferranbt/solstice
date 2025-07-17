// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";

enum AA0 { A, B }
type Price is uint256;

contract TypesTest is Test {
    enum AA1 { A, B }
    type Price1 is uint256;

    AA0 public arg_0;
    AA1 public arg_1;
    Price public arg_2;
    Price1 public arg_3;

    function test_types() external {
        arg_0 = AA0.A;
        arg_1 = AA1.A;
        arg_2 = Price.wrap(100);
        arg_3 = Price1.wrap(200);
    }
}
