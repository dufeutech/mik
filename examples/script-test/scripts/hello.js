// Simple hello script - demonstrates host.call()

export default function(input) {
    // Call the test handler's home endpoint
    var result = host.call("test", {
        method: "GET",
        path: "/"
    });

    // Return the response along with input
    return {
        message: "Hello from script!",
        input: input,
        handler_response: result
    };
}
