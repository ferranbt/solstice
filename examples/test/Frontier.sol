import {Test} from "forge-std/Test.sol";
import {Parent} from "../src/Parent.sol";

contract FooTest is Test {
    struct Example {
        uint256 value_storage;
    }

    uint256 value_storage;
    Example[2][] second_storage;

    function test_Example() external {
        string memory extra = "test strccxxxxxing";

        Parent parent = new Parent();
        parent.setOtherValue(1);
        
        uint256 value = block.number; // Non-constant value
        value = value + 1;
        if (value > 10) {
            value = value + 1;
        }
        value_storage = 1;
        value_storage = 2;
        value_storage = 3;
        value = value + simple_call();
        assert(value != block.number);
    }

    function simple_call() private returns (uint256) {
        return block.number + 2;
    }
}
