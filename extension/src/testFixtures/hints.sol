// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract SimpleCalculator {
    function calculateFactorial(uint256 n) public pure returns (uint256) {
        // Handle edge cases
        if (n == 0 || n == 1) {
            return 1;
        }

        // Check for overflow prevention
        require(n <= 20, "Number too large for factorial");

        // Initialize result
        uint256 result = 1;

        // Calculate factorial using loop
        for (uint256 i = 2; i <= n; i++) {
            // Multiply current result by i
            result = result * i;

            // Optional: check for potential overflow
            require(result >= i, "Overflow detected");
        }

        // Return the final result
        return result;
    }

    // Helper function to demonstrate multi-line formatting
    function helperFunction() internal pure returns (bool) {
        return true;
    }
}
