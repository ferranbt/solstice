// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";

contract A {
    constructor() {}
}

contract B {}

contract C {
    uint256 value;

    constructor() {
        value = 10;
    }
}

contract D is A, B, C {}

contract E is A, B, C {
    uint256 value2;

    constructor() {
        value2 = 20;
    }
}

contract CreateTest is Test {
    function test_contract_create() external {
        A a = new A();
        B b = new B();
        C c = new C();
        D d = new D();
        E e = new E();
    }
}
