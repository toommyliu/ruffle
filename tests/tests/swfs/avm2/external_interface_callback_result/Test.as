package {
    import flash.display.MovieClip;
    import flash.events.UncaughtErrorEvent;
    import flash.external.ExternalInterface;

    public class Test extends MovieClip {
        private var uncaught:int = 0;

        public function Test() {
            loaderInfo.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, function(event:UncaughtErrorEvent):void {
                uncaught++;
                event.preventDefault();
            });
            ExternalInterface.addCallback("returnValue", function(value:*):* { return value; });
            ExternalInterface.addCallback("throwString", function(message:String):void { throw message; });
            ExternalInterface.addCallback("throwError", function():void { throw new Error("error object failed"); });
            ExternalInterface.addCallback("throwUnprintable", function():void {
                throw {toString: function():String { throw "secondary error"; }};
            });
            ExternalInterface.addCallback("uncaughtCount", function():int { return uncaught; });
            stop();
        }
    }
}
