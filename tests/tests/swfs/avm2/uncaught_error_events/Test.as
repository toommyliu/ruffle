package {
    import flash.display.Loader;
    import flash.display.MovieClip;
    import flash.events.Event;
    import flash.events.UncaughtErrorEvent;
    import flash.external.ExternalInterface;
    import flash.net.URLRequest;

    public class Test extends MovieClip {
        private var loader:Loader = new Loader();
        private var child:Object;
        private var step:int = 0;
        private var frames:int = 0;
        private var seen:Array = [];

        public function Test() {
            loaderInfo.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, listener("main loaderInfo"));
            loaderInfo.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, listener("main loaderInfo capture"), true);
            loader.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, listener("Loader"));
            loader.contentLoaderInfo.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, listener("child loaderInfo"));
            loader.contentLoaderInfo.addEventListener(Event.INIT, function(e:Event):void {
                child = loader.content;
                log("Loader.uncaughtErrorEvents is contentLoaderInfo.uncaughtErrorEvents: " + (loader.uncaughtErrorEvents == loader.contentLoaderInfo.uncaughtErrorEvents));
                addChild(loader);
                addEventListener(Event.ENTER_FRAME, onEnterFrame);
            });
            loader.load(new URLRequest("child.swf"));
        }

        private function listener(name:String):Function {
            return function(e:UncaughtErrorEvent):void {
                seen.push(name + " (phase " + e.eventPhase + ", error " + (e.error as Error).message + ")");
            };
        }

        private function log(line:String):void {
            trace(line);
            if (ExternalInterface.available) {
                ExternalInterface.call("report", line);
            }
        }

        private function report(what:String):void {
            log(what + " (frame scripts run " + (child.ran() + MainPart.ran) + "): " + (seen.length == 0 ? "none" : seen.join("; ")));
            seen = [];
        }

        private function onEnterFrame(e:Event):void {
            frames++;
            if (frames % 5 != 0) {
                return;
            }
            switch (step++) {
                case 0:
                    child.frameScriptError();
                    break;
                case 1:
                    report("child frame script on stage");
                    child.enterFrameError();
                    break;
                case 2:
                    report("child enterFrame handler");
                    child.timeoutError();
                    break;
                case 3:
                    report("child setTimeout");
                    child.prepareGoto();
                    break;
                case 4:
                    report("child frame script without an error");
                    var caught:String = "not caught";
                    try {
                        child.gotoWithFrameScriptError();
                    } catch (err:Error) {
                        caught = "caught by the caller: " + err.message;
                    }
                    log("child frame script error in a goto called from main: " + caught);
                    break;
                case 5:
                    report("child frame script in a goto");
                    child.offStageFrameScriptError();
                    break;
                case 6:
                    report("child frame script off stage");
                    MainPart.throwOn = 1;
                    addChild(new MainPart());
                    break;
                case 7:
                    report("main frame script");
                    child.callParent(function():void { throw new Error("main code called by child"); });
                    break;
                case 8:
                    report("main code called by child");
                    removeEventListener(Event.ENTER_FRAME, onEnterFrame);
                    if (ExternalInterface.available) {
                        ExternalInterface.call("report", "done");
                    }
            }
        }
    }
}
