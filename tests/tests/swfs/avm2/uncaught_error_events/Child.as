package {
    import flash.display.MovieClip;
    import flash.events.Event;
    import flash.utils.setTimeout;

    public class Child extends MovieClip {
        private var stopped:MovieClip;
        private var offStage:MovieClip;

        public function ran():int {
            return Part.ran;
        }

        public function frameScriptError():void {
            Part.throwOn = 1;
            addChild(new Part());
        }

        public function enterFrameError():void {
            Part.throwOn = 0;
            addEventListener(Event.ENTER_FRAME, function onFrame(e:Event):void {
                removeEventListener(Event.ENTER_FRAME, onFrame);
                throw new Error("child enterFrame handler");
            });
        }

        public function timeoutError():void {
            setTimeout(function():void { throw new Error("child setTimeout"); }, 1);
        }

        public function prepareGoto():void {
            Part.throwOn = 0;
            stopped = new Part();
            addChild(stopped);
        }

        public function gotoWithFrameScriptError():void {
            Part.throwOn = 2;
            stopped.gotoAndStop(2);
        }

        public function offStageFrameScriptError():void {
            Part.throwOn = 1;
            offStage = new Part();
        }

        public function callParent(fn:Function):void {
            fn();
        }
    }
}
