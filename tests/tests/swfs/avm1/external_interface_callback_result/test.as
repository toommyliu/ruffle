import flash.external.ExternalInterface;

var stringifications = 0;
ExternalInterface.addCallback("stringificationCount", null, function() {
    return stringifications;
});
ExternalInterface.addCallback("returnValue", null, function(value) {
    return value;
});
ExternalInterface.addCallback("throwString", null, function(message) {
    throw message;
});
ExternalInterface.addCallback("throwUnprintable", null, function() {
    throw {toString: function() { stringifications++; throw "secondary error"; }};
});
ExternalInterface.addCallback("returnBadValue", null, function() {
    var reply = {};
    reply.addProperty("value", function() { throw "conversion failed"; }, null);
    return reply;
});
stop();
