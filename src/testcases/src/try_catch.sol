// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

contract TryCatch {
    constructor() {
    }

    function fail(uint256 val) external returns (uint256) {
        if (val == 0) {
            // SUCCESS - val == 3 or any other value
            // Function completes successfully
            
        } else if (val == 1) {
            // ERROR - Require failure with reason
            require(false, "This failed with require");
            
        } else if (val == 2) {
            // PANIC - Division by zero (runtime)
            uint256 zero = 0;
            uint256 result = 100 / zero;
            
        } else {
            // LOW-LEVEL ERROR - Invalid function call (calling non-existent function)
            assembly {
                let success := call(gas(), address(), 0, 0, 0, 0, 0)
                if iszero(success) {
                    revert(0, 0)
                }
            }
        }

        return 1;
    }
}
