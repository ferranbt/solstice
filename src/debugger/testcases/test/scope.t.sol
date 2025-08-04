// SPDX-License-Identifier: UNLICENSED
import {Test} from "forge-std/Test.sol";

contract Complex is Test {
    function test_stack_scope() external {
        uint256 a0 = 9;
        uint256 a1 = 10;
        uint256 a2 = 11;
        uint256 a3 = 12;

        if (a0 < a1) {
            uint256 d0 = 16;
            uint256 d1 = 17;
        }

        for (uint256 i = 0; i < 1; i++) {
            uint256 c1 = 13;
            uint256 c2 = 14;
            uint256 c3 = 15;
        }

        if (a0 < a1) {
            uint256 d0 = 16;
            uint256 d1 = 17;
        }

        // multiple nested scopes
        if (a0 < a1) {
            if (a2 < a3) {
                uint256 e0 = 18;
                uint256 e1 = 19;
            } else {
                // not reached
                uint256 f0 = 20;
                uint256 f1 = 21;
            }
        }

        if (a0 < a1) {
            if (a2 < a3) {
                if (a0 < a1) {
                    uint256 g0 = 22;
                    uint256 g1 = 23;
                }
            }
        }

        // TODO: variables declared after the if statement
        // if (a0 < a1) {
        //     uint256 h0 = 24;
        // }
        // uint256 h1 = 25;
        // uint256 h2 = 26;
    }
}
