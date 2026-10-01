package flash.display {
    import flash.events.EventDispatcher;

    public final class FrameLabel extends EventDispatcher {
        [Ruffle(NativeAccessible)]
        private var _name:String;

        [Ruffle(NativeAccessible)]
        private var _frame:int;

        public function FrameLabel(name:String, frame:int) {
            this._name = name;
            this._frame = frame;
        }

        public function get name():String {
            return this._name;
        }

        public function get frame():int {
            return this._frame;
        }
    }
}
