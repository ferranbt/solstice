// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {TryCatch} from "../src/try_catch.sol";

contract TryCatchTest is Test {
    TryCatch public cc;
    uint256 result; 

    function test_try_catch() external {
        cc = new TryCatch();
        try_fail(0);
        try_fail(1);
        try_fail(2);
        try_fail(3);
        result = 4;
    }

    function try_fail(uint256 value) internal {
        try cc.fail(value) returns (uint256 returnValue)  {
            result = 0;
        } catch Error(string memory reason) {
            result = 1;
        } catch Panic(uint errorCode) {
            result = 2;
        } catch {
            result = 3;
        }
    }
}
